//! Local Kokoro-82M speech synthesis on a dedicated worker thread.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

const MODEL_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
// Full precision: on desktop CPUs the quantized exports run slower than real time.
const MODEL_FILE: &str = "onnx/model.onnx";
const FEMALE_VOICE: &str = "af_heart";
const MALE_VOICE: &str = "am_michael";
const SAMPLE_RATE: u32 = 24_000;
const STYLE_DIM: usize = 256;
/// Voice files hold 510 style rows indexed by phoneme count, so at most 509
/// phonemes fit; with the two boundary pads this stays inside Kokoro's context.
const MAX_PHONEME_TOKENS: usize = 509;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TtsEvent {
    Status(String),
    Speaking(bool),
    Error(String),
    Finished,
}

struct Command {
    id: u64,
    text: String,
    voice: &'static str,
}

struct Kokoro {
    session: ort::session::Session,
    vocab: HashMap<char, i64>,
    /// Per-voice style table: one 256-value row per phoneme count.
    voices: HashMap<&'static str, Vec<f32>>,
}

impl Kokoro {
    fn generate(&mut self, text: &str, voice: &str) -> Result<Vec<f32>, String> {
        use ort::value::Tensor;
        let ipa = kittentts::phonemize::phonemize(text)
            .map_err(|error| format!("Cannot convert text to phonemes: {error:#}"))?;
        let mut ids: Vec<i64> = ipa
            .chars()
            .filter_map(|character| self.vocab.get(&character).copied())
            .collect();
        ids.truncate(MAX_PHONEME_TOKENS);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let styles = self.voices.get(voice).ok_or("Unknown speech voice")?;
        let style = styles
            .get(ids.len() * STYLE_DIM..(ids.len() + 1) * STYLE_DIM)
            .ok_or("The voice file is incomplete. Delete the speech cache and retry.")?
            .to_vec();
        let mut tokens = Vec::with_capacity(ids.len() + 2);
        tokens.push(0);
        tokens.extend(ids);
        tokens.push(0);
        let inputs = (|| {
            Ok::<_, ort::Error>(ort::inputs![
                Tensor::<i64>::from_array(([1usize, tokens.len()], tokens))?,
                Tensor::<f32>::from_array(([1usize, STYLE_DIM], style))?,
                Tensor::<f32>::from_array(([1usize], vec![1.0_f32]))?,
            ])
        })()
        .map_err(|error| format!("Cannot prepare speech input: {error}"))?;
        let outputs = self
            .session
            .run(inputs)
            .map_err(|error| format!("Speech inference failed: {error}"))?;
        let (_, samples) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| format!("Speech model returned no audio: {error}"))?;
        Ok(samples.to_vec())
    }
}

#[derive(Default)]
struct State {
    id: u64,
    available: bool,
    busy: bool,
    status: String,
    error: Option<String>,
    sink: Option<Arc<rodio::Sink>>,
    sender: Option<tokio_mpsc::UnboundedSender<TtsEvent>>,
}

impl State {
    fn emit(&self, event: TtsEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }

    fn set_busy(&mut self, busy: bool) {
        if self.busy != busy {
            self.busy = busy;
            self.emit(TtsEvent::Speaking(busy));
        }
    }

    fn set_status(&mut self, status: String) {
        if self.status != status {
            self.status = status;
            self.emit(TtsEvent::Status(self.status.clone()));
        }
    }

    fn set_error(&mut self, error: String) {
        self.status = error.clone();
        self.error = Some(error.clone());
        self.emit(TtsEvent::Status(error.clone()));
        self.emit(TtsEvent::Error(error));
    }

    fn cancel_current(&mut self) {
        self.id = self.id.wrapping_add(1);
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn stop(&mut self) {
        self.cancel_current();
        self.set_busy(false);
        self.set_status("Ready to read aloud.".into());
    }
}

pub(crate) struct LocalTts {
    commands: mpsc::Sender<Command>,
    state: Arc<Mutex<State>>,
}

impl LocalTts {
    pub(crate) fn new() -> Self {
        Self::with_optional_sender(None)
    }

    pub(crate) fn with_sender(sender: tokio_mpsc::UnboundedSender<TtsEvent>) -> Self {
        Self::with_optional_sender(Some(sender))
    }

    fn with_optional_sender(sender: Option<tokio_mpsc::UnboundedSender<TtsEvent>>) -> Self {
        let (commands, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(State {
            available: true,
            status: "Ready. The voice model loads on first use.".into(),
            sender,
            ..State::default()
        }));
        let worker_state = state.clone();
        // A runtime panic must be visible instead of silently killing speech forever.
        let spawned = std::thread::Builder::new()
            .name("chatty-local-tts".into())
            .spawn(move || {
                if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(receiver, &worker_state);
                })) {
                    let detail = panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown runtime failure");
                    let mut state = worker_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.stop();
                    let msg = format!("Speech worker failed: {detail}. Restart Chatty to retry.");
                    state.set_error(msg);
                }
                worker_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .available = false;
            });
        if let Err(error) = spawned {
            let mut state = state.lock().unwrap();
            let msg = format!("Cannot start local speech: {error}");
            state.set_error(msg);
            state.available = false;
        }
        Self { commands, state }
    }

    pub(crate) fn is_speaking(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    pub(crate) fn status(&self) -> String {
        self.state.lock().unwrap().status.clone()
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.state.lock().unwrap().error.take()
    }

    /// Read `text` aloud with the female or male voice.
    pub(crate) fn speak(&mut self, text: &str, female: bool) -> Result<(), String> {
        let text = speech_text(text);
        if text.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        state.cancel_current();
        state.error = None;
        state.set_busy(true);
        state.set_status("Preparing local speech…".into());
        if self
            .commands
            .send(Command {
                id: state.id,
                text,
                voice: if female { FEMALE_VOICE } else { MALE_VOICE },
            })
            .is_err()
        {
            state.set_busy(false);
            let msg = "The local speech worker stopped. Restart Chatty to retry.".to_string();
            state.set_error(msg.clone());
            return Err(msg);
        }
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.state.lock().unwrap().stop();
    }
}

pub(crate) fn tts_channel() -> (LocalTts, tokio_mpsc::UnboundedReceiver<TtsEvent>) {
    let (sender, receiver) = tokio_mpsc::unbounded_channel();
    (LocalTts::with_sender(sender), receiver)
}

impl Drop for LocalTts {
    fn drop(&mut self) {
        self.stop();
    }
}

fn update_state(state: &Mutex<State>, id: u64, update: impl FnOnce(&mut State)) -> bool {
    let mut state = state.lock().unwrap();
    if state.id != id {
        return false;
    }
    update(&mut state);
    true
}

fn run(commands: mpsc::Receiver<Command>, state: &Mutex<State>) {
    let mut stream = None;
    let mut model = None;
    loop {
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut state = state.lock().unwrap();
                if state.sink.as_ref().is_some_and(|sink| sink.empty()) {
                    state.sink = None;
                    let was_busy = state.busy;
                    state.set_busy(false);
                    state.set_status("Ready to read aloud.".into());
                    if was_busy {
                        state.emit(TtsEvent::Finished);
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let id = command.id;
        if !update_state(state, id, |_| {}) {
            continue;
        }
        let result = (|| -> Result<(), String> {
            if stream.is_none() {
                stream = Some(rodio::OutputStreamBuilder::open_default_stream().map_err(|error| {
                    format!("Cannot open audio output: {error}. Check your output device and volume. Flatpak requires audio permission (pulseaudio).")
                })?);
            }
            if model.is_none() {
                let loaded = load_model(|status| {
                    update_state(state, id, |state| state.set_status(status));
                }).map_err(|error| format!("Cannot load the local voice model: {error:#}. Connect to the internet and try Test voice again."))?;
                model = Some(loaded);
            }
            if !update_state(state, id, |state| {
                state.set_status("Generating speech locally…".into());
            }) {
                return Ok(());
            }
            let sink = Arc::new(rodio::Sink::connect_new(stream.as_ref().unwrap().mixer()));
            if !update_state(state, id, |state| state.sink = Some(sink.clone())) {
                return Ok(());
            }
            // Start playing after the first short chunk; cancel between chunks.
            for chunk in speech_chunks(&command.text) {
                if !update_state(state, id, |_| {}) {
                    return Ok(());
                }
                let samples = model
                    .as_mut()
                    .unwrap()
                    .generate(&chunk, command.voice)
                    .map_err(|error| format!("{error}. Try Test voice again."))?;
                if samples.is_empty() {
                    continue;
                }
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return Err("The voice model returned invalid audio. Try another reply.".into());
                }
                // Check and append under the same lock so Stop cannot race playback.
                if !update_state(state, id, |state| {
                    sink.append(rodio::buffer::SamplesBuffer::new(1, SAMPLE_RATE, samples));
                    state.set_status("Speaking locally…".into());
                }) {
                    return Ok(());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            update_state(state, id, |state| {
                if let Some(sink) = state.sink.take() {
                    sink.stop();
                }
                state.set_busy(false);
                state.set_error(error);
            });
        }
    }
}

fn speech_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for word in text.split_whitespace() {
        if !chunk.is_empty() && chunk.len() + word.len() + 1 > 240 {
            chunks.push(std::mem::take(&mut chunk));
        }
        if !chunk.is_empty() {
            chunk.push(' ');
        }
        chunk.push_str(word);
        if word.ends_with(['.', '!', '?']) {
            chunks.push(std::mem::take(&mut chunk));
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

fn load_model(mut progress: impl FnMut(String)) -> Result<Kokoro, String> {
    // hf-hub's default Api ignores HF_HOME and XDG_CACHE_HOME. In Flatpak that
    // would put models in an ephemeral home folder.
    let mut builder = hf_hub::api::sync::ApiBuilder::from_env().with_progress(false);
    if std::env::var_os("HF_HOME").is_none()
        && let Some(cache) = std::env::var_os("XDG_CACHE_HOME").map(std::path::PathBuf::from)
        && cache.is_absolute()
    {
        builder = builder.with_cache_dir(cache.join("huggingface/hub"));
    }
    let api = builder
        .build()
        .map_err(|error| format!("Cannot initialize model download: {error}"))?;
    let repo = api.model(MODEL_REPO.into());
    let files = [
        "tokenizer.json".to_owned(),
        format!("voices/{FEMALE_VOICE}.bin"),
        format!("voices/{MALE_VOICE}.bin"),
        MODEL_FILE.to_owned(),
    ];
    let mut paths = Vec::with_capacity(files.len());
    for (step, file) in files.iter().enumerate() {
        progress(format!(
            "Preparing voice model ({}/{}): {file}. First use downloads about 330 MB.",
            step + 1,
            files.len()
        ));
        paths.push(
            repo.get(file)
                .map_err(|error| format!("Cannot fetch {file}: {error}"))?,
        );
    }
    progress("Loading local voice model…".into());
    let read = |path: &std::path::Path| {
        std::fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))
    };
    let tokenizer: serde_json::Value = serde_json::from_slice(&read(&paths[0])?)
        .map_err(|error| format!("Invalid speech tokenizer: {error}"))?;
    let vocab: HashMap<char, i64> = tokenizer["model"]["vocab"]
        .as_object()
        .ok_or("Speech tokenizer has no vocabulary")?
        .iter()
        .filter_map(|(symbol, id)| {
            let mut chars = symbol.chars();
            let character = chars.next()?;
            chars.next().is_none().then_some((character, id.as_i64()?))
        })
        .collect();
    let mut voices = HashMap::new();
    for (voice, path) in [(FEMALE_VOICE, &paths[1]), (MALE_VOICE, &paths[2])] {
        let bytes = read(path)?;
        if bytes.len() < (MAX_PHONEME_TOKENS + 1) * STYLE_DIM * 4 {
            return Err(format!("Voice file {voice} is incomplete"));
        }
        let (styles, _) = bytes.as_chunks::<4>();
        let styles = styles.iter().copied().map(f32::from_le_bytes).collect();
        voices.insert(voice, styles);
    }
    let session = ort::session::Session::builder()
        .and_then(|builder| builder.commit_from_file(&paths[3]))
        .map_err(|error| format!("Cannot initialize local speech model: {error}"))?;
    Ok(Kokoro {
        session,
        vocab,
        voices,
    })
}

pub(crate) fn test_speech() -> Result<(), String> {
    let mut tts = LocalTts::new();
    tts.speak("Hello! Chatty's local speech is working.", true)?;
    let started = std::time::Instant::now();
    let mut last_status = String::new();
    loop {
        if let Some(error) = tts.take_error() {
            return Err(error);
        }
        let status = tts.status();
        if status != last_status {
            println!("{status}");
            last_status = status;
        }
        if !tts.is_speaking() {
            // Errors and completion are published together by the worker.
            if let Some(error) = tts.take_error() {
                return Err(error);
            }
            println!("Local speech synthesis and playback completed.");
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(120) {
            tts.stop();
            return Err("Local speech test timed out after 120 seconds.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether a character's free-text gender selects the female voice. Anything
/// else, including an empty field, uses the male voice.
pub(crate) fn is_female(gender: &str) -> bool {
    let gender = gender.trim().to_lowercase();
    matches!(
        gender.as_str(),
        "f" | "female" | "woman" | "girl" | "lady" | "mare" | "filly" | "feminine" | "she"
    ) || gender.starts_with("she/")
        || gender.starts_with("female")
}

/// Quoted dialogue in a roleplay reply, joined in order. Narration and
/// actions outside straight or curly double quotes are skipped; an unclosed
/// quote runs to the end of the reply.
pub(crate) fn dialogue(text: &str) -> String {
    let mut spoken = Vec::new();
    let mut current = String::new();
    let mut closing = None;
    for character in text.chars() {
        match (closing, character) {
            (None, '"') => closing = Some('"'),
            (None, '\u{201c}') => closing = Some('\u{201d}'),
            (None, _) => {}
            (Some(close), character) if character == close => {
                spoken.push(std::mem::take(&mut current));
                closing = None;
            }
            (Some(_), character) => current.push(character),
        }
    }
    spoken.push(current);
    spoken
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Plain text for speech. Punctuation is kept because it drives pauses and
/// intonation; markdown emphasis, roleplay action asterisks, and typographic
/// quotes and dashes are reduced to forms the phonemizer reads naturally.
fn speech_text(markdown: &str) -> String {
    let mut text = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        let line = line.trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '#' | '>' | '-' | '*' | '`')
        });
        if line.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        for character in line.chars() {
            match character {
                '*' | '`' => {}
                '\u{2018}' | '\u{2019}' => text.push('\''),
                '\u{201c}' | '\u{201d}' => text.push('"'),
                '\u{2013}' | '\u{2014}' => text.push_str(", "),
                '\u{2026}' => text.push_str("..."),
                character => text.push(character),
            }
        }
    }
    text.replace("__", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" ,", ",")
}
