//! In-process emotion classification for reply portraits.
//!
//! Replies are persisted with the default portrait first, so chat never waits
//! on classification. A background task then runs the configured GGUF
//! classifier (JusteLeo/emotion-text-classifier-LLM) on a blocking thread and
//! publishes a message update when a better portrait matches.

use super::*;
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{LlamaModel, params::LlamaModelParams},
    sampling::LlamaSampler,
};
use std::num::NonZeroU32;

/// System turn baked into the classifier's chat template. llama.cpp does not
/// execute Jinja templates, so the prompt is assembled here verbatim.
const SYSTEM_PROMPT: &str = "You are an emotion classification assistant. Your task is to analyze ALL given sentence and classify it emotions chosen from Contentment, Joy, Euphoria, Excitement, Disappointment, Sadness, Regret, Irritation, Frustration, Anger, Anxiety, Fear, Astonishment, Disgust, Hate, Pleasure, Desire, Affection, Trust, Distrust, Gratitude, Compassion, Admiration, Contempt, Guilt, Shame, Pride, Jealousy, Envy, Hope, Nostalgia, Relief, Curiosity, Boredom, Neutral, fatigue, Trust You can choose one or several emotions follow this format\n```json\n{\n  \"emotions\": [ \" \"\n  ],\n  \"explanation\": \"This is the explanation related to the listed emotions.\"\n}\n```\nbegin";
const CONTEXT_TOKENS: u32 = 2048;
/// Keeps the prompt plus output inside [`CONTEXT_TOKENS`] even at one token
/// per character.
const MAX_INPUT_CHARS: usize = 1536;
/// The emotion list closes well within this; the explanation is not decoded.
const MAX_OUTPUT_TOKENS: usize = 64;
/// Portraits are cosmetic, so replies beyond this backlog keep the default.
const MAX_PENDING: usize = 8;

/// Classifier emotions mapped to the portrait labels the GUI generates.
const PORTRAIT_LABELS: [(&str, &str); 36] = [
    ("contentment", "happy"),
    ("joy", "happy"),
    ("euphoria", "laughing"),
    ("excitement", "happy"),
    ("pleasure", "happy"),
    ("gratitude", "happy"),
    ("admiration", "happy"),
    ("hope", "happy"),
    ("relief", "happy"),
    ("trust", "happy"),
    ("disappointment", "sad"),
    ("sadness", "sad"),
    ("regret", "sad"),
    ("compassion", "sad"),
    ("irritation", "angry"),
    ("frustration", "angry"),
    ("anger", "angry"),
    ("hate", "angry"),
    ("jealousy", "angry"),
    ("envy", "angry"),
    ("anxiety", "scared"),
    ("fear", "scared"),
    ("astonishment", "surprised"),
    ("disgust", "disgusted"),
    ("desire", "flirty"),
    ("affection", "flirty"),
    ("contempt", "smug"),
    ("pride", "smug"),
    ("guilt", "embarrassed"),
    ("shame", "embarrassed"),
    ("distrust", "thoughtful"),
    ("nostalgia", "thoughtful"),
    ("curiosity", "thoughtful"),
    ("boredom", "neutral"),
    ("fatigue", "neutral"),
    ("neutral", "neutral"),
];

struct Runtime {
    backend: LlamaBackend,
    /// Loaded lazily and reloaded when the configured path changes.
    model: std::sync::Mutex<Option<(String, LlamaModel)>>,
    pending: Arc<Semaphore>,
}

/// llama.cpp allows one backend per process, so the runtime is process-wide.
static RUNTIME: std::sync::OnceLock<Option<Runtime>> = std::sync::OnceLock::new();

fn runtime() -> Option<&'static Runtime> {
    RUNTIME
        .get_or_init(|| match LlamaBackend::init() {
            Ok(mut backend) => {
                backend.void_logs();
                Some(Runtime {
                    backend,
                    model: std::sync::Mutex::new(None),
                    pending: Arc::new(Semaphore::new(MAX_PENDING)),
                })
            }
            Err(error) => {
                warn!(%error, "emotion classifier backend unavailable");
                None
            }
        })
        .as_ref()
}

pub(super) struct PortraitJob {
    pub owner_id: String,
    pub message_id: String,
    pub generation_id: String,
    pub reply: String,
    /// Portrait `(id, label)` pairs; image bytes are not retained.
    pub portraits: Vec<(String, String)>,
    pub current_image_id: Option<String>,
}

/// Queue classification of a persisted reply without blocking the caller.
pub(super) fn schedule_portrait_classification(app: &App, model_path: &str, job: PortraitJob) {
    if model_path.is_empty() || job.portraits.len() <= 1 || job.reply.trim().is_empty() {
        return;
    }
    let Some(runtime) = runtime() else { return };
    let Ok(permit) = runtime.pending.clone().try_acquire_owned() else {
        debug!(message_id = %job.message_id, "emotion classifier busy; keeping the default portrait");
        return;
    };
    let app = app.clone();
    let model_path = model_path.to_owned();
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = classify_and_persist(&app, runtime, model_path, job).await {
            warn!(%error, "emotion classification failed; keeping the default portrait");
        }
    });
}

async fn classify_and_persist(
    app: &App,
    runtime: &'static Runtime,
    model_path: String,
    job: PortraitJob,
) -> Result<()> {
    let reply = clip(&job.reply, MAX_INPUT_CHARS);
    let emotions =
        tokio::task::spawn_blocking(move || classify(runtime, &model_path, &reply)).await??;
    debug!(message_id = %job.message_id, ?emotions, "emotion classifier result");
    let Some(image_id) = portrait_for_emotions(&job.portraits, &emotions) else {
        return Ok(());
    };
    if job.current_image_id.as_deref() == Some(image_id) {
        return Ok(());
    }
    if let Some(delta) = persist_portrait(app, &job, image_id).await? {
        let _ = app.deltas.send(PublishedDelta {
            owner_id: job.owner_id,
            origin: String::new(),
            encoded: encode(&delta)?.into(),
        });
    }
    Ok(())
}

/// Run the classifier to the end of its emotion list. Holds the model lock,
/// so classifications run one at a time.
fn classify(runtime: &Runtime, model_path: &str, text: &str) -> Result<Vec<String>> {
    let mut slot = runtime
        .model
        .lock()
        .map_err(|_| format_err!("emotion classifier lock poisoned"))?;
    if slot.as_ref().is_none_or(|(loaded, _)| loaded != model_path) {
        // Free the previous weights before loading replacements.
        *slot = None;
        let model =
            LlamaModel::load_from_file(&runtime.backend, model_path, &LlamaModelParams::default())
                .with_context(|| format!("load emotion classifier {model_path}"))?;
        *slot = Some((model_path.to_owned(), model));
    }
    let (_, model) = slot.as_ref().context("emotion classifier missing")?;
    let vocab = model.vocab();

    // Reply text is tokenized without special-token parsing so it cannot
    // close the user turn or inject a new one.
    let mut tokens = vocab.tokenize(
        format!("<start_of_turn>system\n{SYSTEM_PROMPT}<end_of_turn>\n<start_of_turn>user\n")
            .as_bytes(),
        true,
        true,
    );
    tokens.extend(vocab.tokenize(text.trim().as_bytes(), false, false));
    tokens.extend(vocab.tokenize(b"<end_of_turn>\n<start_of_turn>model\n", false, true));
    if tokens.len() + MAX_OUTPUT_TOKENS > CONTEXT_TOKENS as usize {
        bail!("emotion classifier prompt exceeds its context window")
    }

    let threads =
        std::thread::available_parallelism().map_or(2, |count| (count.get() / 2).max(1)) as i32;
    let mut context = model.new_context(
        &runtime.backend,
        LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(CONTEXT_TOKENS))
            .with_n_batch(CONTEXT_TOKENS)
            .with_n_threads(threads)
            .with_n_threads_batch(threads),
    )?;
    let mut batch = LlamaBatch::new(CONTEXT_TOKENS as usize, 1);
    let last = tokens.len() - 1;
    for (position, token) in tokens.iter().enumerate() {
        batch.add(*token, position as i32, &[0], position == last)?;
    }
    context.decode(&mut batch)?;

    let mut sampler = LlamaSampler::greedy();
    let mut output = Vec::new();
    let start = tokens.len() as i32;
    for position in start..start + MAX_OUTPUT_TOKENS as i32 {
        let token = sampler.sample(&context, batch.n_tokens() - 1);
        sampler.accept(token);
        if vocab.is_eog(token) {
            break;
        }
        output.extend(vocab.token_to_piece(token, false, None));
        if let Some(emotions) = parse_emotions(&String::from_utf8_lossy(&output)) {
            return Ok(emotions);
        }
        batch.clear();
        batch.add(token, position, &[0], true)?;
        context.decode(&mut batch)?;
    }
    parse_emotions(&String::from_utf8_lossy(&output))
        .context("emotion classifier returned no emotion list")
}

/// Extract the `emotions` array once the model has closed it.
fn parse_emotions(output: &str) -> Option<Vec<String>> {
    let start = output.find("\"emotions\"")?;
    let list = &output[start..];
    let open = list.find('[')?;
    let close = open + list[open..].find(']')?;
    let emotions = serde_json::from_str::<Vec<String>>(&list[open..=close]).ok()?;
    Some(
        emotions
            .into_iter()
            .map(|emotion| emotion.trim().to_owned())
            .filter(|emotion| !emotion.is_empty())
            .collect(),
    )
}

/// First emotion, in model order, that names a portrait label directly or
/// through [`PORTRAIT_LABELS`].
pub(super) fn portrait_for_emotions<'a>(
    portraits: &'a [(String, String)],
    emotions: &[String],
) -> Option<&'a str> {
    let find = |label: &str| {
        portraits
            .iter()
            .find(|(_, name)| name.trim().eq_ignore_ascii_case(label))
            .map(|(id, _)| id.as_str())
    };
    emotions.iter().find_map(|emotion| {
        find(emotion).or_else(|| {
            PORTRAIT_LABELS
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(emotion))
                .and_then(|(_, label)| find(label))
        })
    })
}

/// Store the classified portrait when the reply is still the one shown.
async fn persist_portrait(
    app: &App,
    job: &PortraitJob,
    image_id: &str,
) -> Result<Option<StateDelta>> {
    let mut transaction = app.db.begin().await?;
    let Some(row) = sqlx::query(
        "SELECT m.conversation_id,m.author_type,m.author_id,m.content,m.parent_id,m.selected_variant_id,m.character_image_id FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE m.id=? AND c.owner_id=?",
    )
    .bind(&job.message_id)
    .bind(&job.owner_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        return Ok(None);
    };
    let selected_variant_id: Option<String> = row.get("selected_variant_id");
    // A regenerated reply only owns the portrait while it stays selected.
    if job.generation_id != job.message_id
        && selected_variant_id.as_deref() != Some(job.generation_id.as_str())
    {
        return Ok(None);
    }
    if row.get::<Option<String>, _>("character_image_id") != job.current_image_id {
        return Ok(None);
    }
    let changed = encode(&DeltaPayload::Message {
        conversation_id: row.get("conversation_id"),
        author_type: row.get("author_type"),
        author_id: row.get("author_id"),
        content: row.get("content"),
        parent_id: row.get("parent_id"),
        selected_variant_id,
        character_image_id: Some(image_id.to_owned()),
    })?;
    let revision = delta_tx(
        &mut transaction,
        &job.owner_id,
        "message",
        &job.message_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE messages SET character_image_id=?,revision=? WHERE id=?")
        .bind(image_id)
        .bind(revision)
        .bind(&job.message_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(Some(StateDelta {
        revision,
        entity_type: "message".into(),
        entity_id: job.message_id.clone(),
        operation: DeltaOperation::Update,
        changed_fields: changed,
    }))
}

/// Recommended classifier, pinned to a reviewed revision and verified by hash.
const RECOMMENDED_MODEL_URL: &str = "https://huggingface.co/JusteLeo/emotion-text-classifier-LLM/resolve/bef09f442f532bcd438a803de6877d7309e279ce/EmotionTextClassifierLLM.gguf";
const RECOMMENDED_MODEL_FILE: &str = "EmotionTextClassifierLLM.gguf";
const RECOMMENDED_MODEL_BYTES: u64 = 720_424_992;
const RECOMMENDED_MODEL_SHA256: &str =
    "fe8f3aa8e7b7f5662cc6c18ccb04b10a364197251b8920656759b7bb37cf1a50";
/// One install at a time; a second request fails instead of racing the file.
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct InstallGuard;
impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::Release);
    }
}

/// Download the recommended classifier into `<data dir>/models` and return its
/// path. An existing verified copy is reused. The download streams to a
/// `.partial` file that replaces the target only after size and SHA-256 match,
/// so an interrupted install never leaves a truncated model selected.
/// Dropping the future (for example on disconnect) abandons the partial file.
pub(super) async fn install_recommended_model(
    tx: &mpsc::Sender<Out>,
    request_id: u64,
) -> Result<PathBuf> {
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    if INSTALLING.swap(true, Ordering::AcqRel) {
        bail!("the emotion classifier is already being downloaded")
    }
    let _guard = InstallGuard;
    let dir = default_user_data_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .context("could not determine the broker data directory; set XDG_DATA_HOME or HOME")?
    .join("models");
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create model directory {}", dir.display()))?;
    let target = dir.join(RECOMMENDED_MODEL_FILE);
    let progress = |status: &str, completed: Option<u64>| -> Result<Out> {
        Ok((
            MessageType::Response,
            request_id,
            encode(&Response::ModelDownloadProgress(
                chatty_protocol::admin::ModelDownloadProgress {
                    status: status.into(),
                    digest: None,
                    total: Some(RECOMMENDED_MODEL_BYTES),
                    completed,
                },
            ))?
            .into(),
        ))
    };
    if tokio::fs::metadata(&target)
        .await
        .is_ok_and(|metadata| metadata.len() == RECOMMENDED_MODEL_BYTES)
    {
        tx.send(progress("Verifying existing download", None)?)
            .await?;
        let path = target.clone();
        let digest = tokio::task::spawn_blocking(move || -> Result<String> {
            let mut file = std::fs::File::open(&path)?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(format!("{:x}", hasher.finalize()))
        })
        .await??;
        if digest == RECOMMENDED_MODEL_SHA256 {
            return Ok(target);
        }
    }
    let partial = dir.join(format!("{RECOMMENDED_MODEL_FILE}.partial"));
    let response = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()?
        .get(RECOMMENDED_MODEL_URL)
        .send()
        .await?
        .error_for_status()
        .context("emotion classifier download failed")?;
    if response
        .content_length()
        .is_some_and(|length| length != RECOMMENDED_MODEL_BYTES)
    {
        bail!("emotion classifier download has an unexpected size")
    }
    let mut file = tokio::fs::File::create(&partial)
        .await
        .with_context(|| format!("create {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut completed = 0_u64;
    let mut last_sent = None::<Instant>;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.context("emotion classifier download interrupted")?;
        completed += chunk.len() as u64;
        if completed > RECOMMENDED_MODEL_BYTES {
            bail!("emotion classifier download is larger than expected")
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        if last_sent.is_none_or(|last| last.elapsed() >= Duration::from_millis(250)) {
            tx.send(progress("Downloading emotion classifier", Some(completed))?)
                .await?;
            last_sent = Some(Instant::now());
        }
    }
    file.sync_all().await?;
    drop(file);
    if completed != RECOMMENDED_MODEL_BYTES
        || format!("{:x}", hasher.finalize()) != RECOMMENDED_MODEL_SHA256
    {
        let _ = tokio::fs::remove_file(&partial).await;
        bail!("emotion classifier download failed verification; try again")
    }
    tokio::fs::rename(&partial, &target).await?;
    tx.send(progress("Download complete", Some(completed))?)
        .await?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portraits() -> Vec<(String, String)> {
        ["neutral", "happy", "angry", "Joy"]
            .iter()
            .map(|label| (format!("portrait-{label}"), (*label).to_owned()))
            .collect()
    }

    #[test]
    fn parses_the_emotion_list_once_it_closes() {
        assert_eq!(
            parse_emotions("```json\n{\n  \"emotions\": [\"Anger\","),
            None
        );
        assert_eq!(
            parse_emotions(
                "```json\n{\n  \"emotions\": [ \"Anger\", \" Irritation \" ],\n  \"expl"
            ),
            Some(vec!["Anger".to_owned(), "Irritation".to_owned()])
        );
        assert_eq!(parse_emotions("{\"emotions\": [3]}"), None);
    }

    #[test]
    fn maps_emotions_to_portraits_in_model_order() {
        let portraits = portraits();
        let pick = |emotions: &[&str]| {
            let emotions = emotions.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>();
            portrait_for_emotions(&portraits, &emotions).map(str::to_owned)
        };
        // A custom label matching the classifier's emotion wins over the map.
        assert_eq!(pick(&["joy"]).as_deref(), Some("portrait-Joy"));
        assert_eq!(pick(&["Frustration"]).as_deref(), Some("portrait-angry"));
        // Emotions without a portrait fall through to the next one.
        assert_eq!(
            pick(&["Fear", "Contentment"]).as_deref(),
            Some("portrait-happy")
        );
        assert_eq!(pick(&["Fear"]), None);
        assert_eq!(pick(&[]), None);
    }

    #[tokio::test]
    async fn portrait_updates_only_the_unchanged_visible_reply() {
        let app =
            crate::inference::auxiliary_completion_tests::test_app("http://127.0.0.1:9").await;
        for (id, parent, selected, image) in [
            ("reply", None::<&str>, None::<&str>, "portrait-neutral"),
            ("edited", None, None, "portrait-angry"),
            ("parent", None, Some("old-variant"), "portrait-neutral"),
        ] {
            sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,parent_id,selected_variant_id,character_image_id,revision) VALUES(?,'chat','character','c1','hi',?,?,?,1)")
                .bind(id).bind(parent).bind(selected).bind(image)
                .execute(&app.db).await.unwrap();
        }
        let job = |message_id: &str, generation_id: &str| PortraitJob {
            owner_id: "owner".into(),
            message_id: message_id.into(),
            generation_id: generation_id.into(),
            reply: "hi".into(),
            portraits: portraits(),
            current_image_id: Some("portrait-neutral".into()),
        };
        let image = |id: &str| {
            let db = app.db.clone();
            let id = id.to_owned();
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT character_image_id FROM messages WHERE id=?",
                )
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap()
            }
        };

        let delta = persist_portrait(&app, &job("reply", "reply"), "portrait-happy")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(delta.operation, DeltaOperation::Update));
        let Ok(DeltaPayload::Message {
            character_image_id, ..
        }) = decode::<DeltaPayload>(&delta.changed_fields)
        else {
            panic!("message delta expected")
        };
        assert_eq!(character_image_id.as_deref(), Some("portrait-happy"));
        assert_eq!(image("reply").await, "portrait-happy");

        // A portrait changed since the reply was saved is left alone.
        assert!(
            persist_portrait(&app, &job("edited", "edited"), "portrait-happy")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(image("edited").await, "portrait-angry");
        // A variant no longer selected does not own the message portrait.
        assert!(
            persist_portrait(&app, &job("parent", "stale-variant"), "portrait-happy")
                .await
                .unwrap()
                .is_none()
        );
        // Other owners and deleted messages are ignored.
        let mut foreign = job("reply", "reply");
        foreign.owner_id = "intruder".into();
        foreign.current_image_id = Some("portrait-happy".into());
        assert!(
            persist_portrait(&app, &foreign, "portrait-angry")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            persist_portrait(&app, &job("gone", "gone"), "portrait-happy")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Needs `CHATTY_EMOTION_MODEL` pointing at EmotionTextClassifierLLM.gguf.
    #[test]
    #[ignore]
    fn live_classifier_picks_distinct_portraits() {
        let path = std::env::var("CHATTY_EMOTION_MODEL").unwrap();
        let runtime = runtime().unwrap();
        let portraits = portraits();
        for (reply, expected) in [
            (
                "*beams and claps* This is the best day of my life!",
                "portrait-Joy",
            ),
            (
                "*slams the table* How dare you lie to me again!",
                "portrait-angry",
            ),
        ] {
            let emotions = classify(runtime, &path, reply).unwrap();
            assert_eq!(
                portrait_for_emotions(&portraits, &emotions),
                Some(expected),
                "{emotions:?}"
            );
        }
    }

    #[test]
    fn every_mapped_label_is_a_generated_portrait_label() {
        const GENERATED: [&str; 12] = [
            "neutral",
            "happy",
            "laughing",
            "sad",
            "angry",
            "surprised",
            "scared",
            "embarrassed",
            "flirty",
            "thoughtful",
            "smug",
            "disgusted",
        ];
        for (emotion, label) in PORTRAIT_LABELS {
            assert!(
                GENERATED.contains(&label),
                "{emotion} maps to unknown {label}"
            );
            assert!(SYSTEM_PROMPT.to_lowercase().contains(emotion), "{emotion}");
        }
    }
}
