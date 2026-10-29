use super::*;

pub(super) const MAX_ADMIN_LOG_RESPONSE_BYTES: u64 = 512 * 1024;

pub(super) fn read_broker_log(path: &Path) -> Result<BrokerLog> {
    let mut file =
        File::open(path).with_context(|| format!("open broker boot log {}", path.display()))?;
    let file_size_bytes = file.metadata()?.len();
    let start = file_size_bytes.saturating_sub(MAX_ADMIN_LOG_RESPONSE_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(file_size_bytes.min(MAX_ADMIN_LOG_RESPONSE_BYTES)).unwrap_or(0),
    );
    file.take(MAX_ADMIN_LOG_RESPONSE_BYTES)
        .read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }
    Ok(BrokerLog {
        content: String::from_utf8_lossy(&bytes).into_owned(),
        truncated: start > 0,
        file_size_bytes,
    })
}

pub(super) async fn broker_monitor(app: &App) -> BrokerMonitor {
    let memory_used_mb = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0)
        / 1024;
    let memory_limit_mb = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value < u64::MAX / 2)
        .map(|bytes| bytes / 1024 / 1024);
    let uptime_seconds = STARTED_AT
        .get()
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0);
    let cpu_ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let fields = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .collect::<Vec<_>>();
            let user = fields.get(11)?.parse::<u64>().ok()?;
            let system = fields.get(12)?.parse::<u64>().ok()?;
            Some(user + system)
        })
        .unwrap_or(0);
    let now = std::time::Instant::now();
    let cpu_percent = CPU_SAMPLE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .map(|mut sample| {
            let percent = sample
                .as_ref()
                .map(|(previous_at, previous_ticks)| {
                    let elapsed = now.duration_since(*previous_at).as_secs_f32();
                    if elapsed > 0.0 {
                        cpu_ticks.saturating_sub(*previous_ticks) as f32 / 100.0 / elapsed * 100.0
                    } else {
                        0.0
                    }
                })
                .unwrap_or_else(|| cpu_ticks as f32 / 100.0 / uptime_seconds.max(1) as f32 * 100.0);
            *sample = Some((now, cpu_ticks));
            percent
        })
        .unwrap_or(0.0);
    let (adapter_status, adapter_model_count, adapter_latency_ms) = adapter_health(app).await;
    let recent_errors = app.recent_errors.lock().await.clone();
    BrokerMonitor {
        uptime_seconds,
        cpu_percent,
        memory_used_mb,
        memory_limit_mb,
        active_connections: ACTIVE_CONNECTIONS.load(Ordering::Relaxed),
        adapter_status,
        adapter_model_count,
        adapter_latency_ms,
        recent_errors,
    }
}

pub(super) async fn adapter_health(app: &App) -> (AdapterStatus, u32, Option<u64>) {
    let Ok(config) = load_broker_config(&app.db).await else {
        return (AdapterStatus::Offline, 0, None);
    };
    if !config.adapter_enabled {
        return (AdapterStatus::Disabled, 0, None);
    }
    let started = std::time::Instant::now();
    let result = async {
        app.adapters
            .inference(&config)?
            .models(Duration::from_secs(2))
            .await
            .map_err(Error::from)
    }
    .await;
    let Ok(models) = result else {
        return (
            AdapterStatus::Offline,
            0,
            Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        );
    };
    let models = models.len() as u32;
    let status = if models > 0 {
        AdapterStatus::Online
    } else {
        AdapterStatus::Offline
    };
    (
        status,
        models,
        Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
    )
}

pub(super) async fn load_broker_config(db: &SqlitePool) -> Result<BrokerConfig> {
    let row = sqlx::query("SELECT adapter_enabled,adapter_url,adapter_plugin,use_ollama_api,model,temperature,top_p,top_k,num_ctx,num_predict,repeat_penalty,seed,keep_alive,allow_public_characters,allow_self_registration,image_enabled,image_url,image_plugin,image_model,image_width,image_height,image_steps,image_guidance,image_negative_prompt,image_workflow,emotion_model,reasoning,memory_model FROM broker_settings WHERE singleton=1")
        .fetch_one(db)
        .await?;
    Ok(BrokerConfig {
        adapter_enabled: row.get("adapter_enabled"),
        adapter_url: row.get("adapter_url"),
        adapter_plugin: row.get("adapter_plugin"),
        use_ollama_api: row.get("use_ollama_api"),
        model: row.get("model"),
        temperature: row.get("temperature"),
        top_p: row.get("top_p"),
        top_k: row.get::<i64, _>("top_k") as u32,
        num_ctx: row.get::<i64, _>("num_ctx") as u32,
        num_predict: row.get::<i64, _>("num_predict") as i32,
        repeat_penalty: row.get("repeat_penalty"),
        seed: row.get("seed"),
        keep_alive: row.get("keep_alive"),
        allow_public_characters: row.get("allow_public_characters"),
        allow_self_registration: row.get("allow_self_registration"),
        image: ImageConfig {
            enabled: row.get("image_enabled"),
            url: row.get("image_url"),
            plugin: row.get("image_plugin"),
            model: row.get("image_model"),
            width: row.get::<i64, _>("image_width") as u32,
            height: row.get::<i64, _>("image_height") as u32,
            steps: row.get::<i64, _>("image_steps") as u32,
            guidance: row.get("image_guidance"),
            negative_prompt: row.get("image_negative_prompt"),
            workflow: row.get("image_workflow"),
        },
        emotion_model: row.get("emotion_model"),
        memory_model: row.get("memory_model"),
        // The CHECK constraint keeps stored names valid.
        reasoning: ReasoningLevel::from_name(row.get("reasoning")).unwrap_or_default(),
    })
}

pub(super) fn validate_broker_config(config: &BrokerConfig) -> Result<()> {
    if config.model.len() > 256 {
        bail!("model name must not exceed 256 bytes")
    }
    if !config.temperature.is_finite() || !(0.0..=2.0).contains(&config.temperature) {
        bail!("temperature must be between 0 and 2")
    }
    if !config.top_p.is_finite() || !(0.0..=1.0).contains(&config.top_p) {
        bail!("top-p must be between 0 and 1")
    }
    if config.top_k > 10_000 {
        bail!("top-k must be between 0 and 10000")
    }
    if !(128..=1_048_576).contains(&config.num_ctx) {
        bail!("context length must be between 128 and 1048576")
    }
    if config.num_predict < -1 || config.num_predict > 1_048_576 {
        bail!("prediction limit must be -1 or between 0 and 1048576")
    }
    if !config.repeat_penalty.is_finite() || !(0.0..=2.0).contains(&config.repeat_penalty) {
        bail!("repeat penalty must be between 0 and 2")
    }
    if config.keep_alive.is_empty() || config.keep_alive.len() > 32 {
        bail!("keep-alive must be an Ollama duration such as 5m, 1h, or 0")
    }
    if config.emotion_model.len() > 4096 || config.emotion_model.chars().any(char::is_control) {
        bail!("emotion model path is invalid")
    }
    if config.memory_model.len() > 256 || config.memory_model.chars().any(char::is_control) {
        bail!("memory model name is invalid")
    }
    let image = &config.image;
    if !image.url.starts_with("http://") && !image.url.starts_with("https://") {
        bail!("image service URL must use http or https")
    }
    if image.model.len() > 256 || image.model.chars().any(char::is_control) {
        bail!("image model name is invalid")
    }
    let sides = chatty_protocol::admin::MIN_IMAGE_SIDE..=chatty_protocol::admin::MAX_IMAGE_SIDE;
    if !sides.contains(&image.width)
        || !sides.contains(&image.height)
        || !image.width.is_multiple_of(8)
        || !image.height.is_multiple_of(8)
    {
        bail!("image size must be 256 to 2048 pixels per side, in multiples of 8")
    }
    if !(1..=chatty_protocol::admin::MAX_IMAGE_STEPS).contains(&image.steps) {
        bail!("image steps must be between 1 and 150")
    }
    if !image.guidance.is_finite() || !(0.0..=30.0).contains(&image.guidance) {
        bail!("image guidance must be between 0 and 30")
    }
    if image.negative_prompt.len() > chatty_protocol::admin::MAX_IMAGE_PROMPT_BYTES {
        bail!("image negative prompt must not exceed 4096 bytes")
    }
    validate_image_workflow(&image.workflow)
}

/// Check a ComfyUI API-format template before it is saved, so mistakes surface
/// in the admin portal instead of during a user's image request.
pub(super) fn validate_image_workflow(workflow: &str) -> Result<()> {
    if workflow.is_empty() {
        return Ok(());
    }
    if workflow.len() > chatty_protocol::admin::MAX_IMAGE_WORKFLOW_BYTES {
        bail!("image workflow must not exceed 256 KiB")
    }
    let graph: Value =
        serde_json::from_str(workflow).context("image workflow is not valid JSON")?;
    let Some(nodes) = graph.as_object() else {
        bail!("image workflow must be a JSON object of nodes")
    };
    if nodes.contains_key("nodes") && nodes.contains_key("links") {
        bail!("this is a UI workflow; in ComfyUI use Workflow > Export (API)")
    }
    if nodes.is_empty() || nodes.len() > 500 {
        bail!("image workflow must contain 1 to 500 nodes")
    }
    for (id, node) in nodes {
        if !node["class_type"].is_string() || !node["inputs"].is_object() {
            bail!("workflow node {id} needs a class_type and inputs")
        }
    }
    if !workflow.contains("{{prompt}}") {
        bail!("image workflow must use {{{{prompt}}}} in a text input")
    }
    let mut rest = workflow;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start..].find("}}") else {
            break;
        };
        let placeholder = &rest[start..start + length + 2];
        if !chatty_protocol::admin::IMAGE_WORKFLOW_PLACEHOLDERS.contains(&placeholder) {
            bail!("unknown image workflow placeholder {placeholder}")
        }
        rest = &rest[start + length + 2..];
    }
    Ok(())
}

pub(super) async fn load_ollama_state(app: &App) -> Result<OllamaState> {
    let config = load_broker_config(&app.db).await?;
    let state = app.adapters.management(&config)?.model_state().await?;
    Ok(OllamaState {
        version: state.version,
        models: state
            .models
            .into_iter()
            .map(|model| OllamaModel {
                name: model.name,
                size: model.size,
                modified_at: model.modified_at,
                family: model.family,
                parameter_size: model.parameter_size,
                quantization_level: model.quantization_level,
            })
            .collect(),
        running_models: state
            .running_models
            .into_iter()
            .map(|model| OllamaRunningModel {
                name: model.name,
                size: model.size,
                size_vram: model.size_vram,
                expires_at: model.expires_at,
            })
            .collect(),
    })
}

pub(super) async fn run_ollama_action(app: &App, action: OllamaAction) -> Result<()> {
    let config = load_broker_config(&app.db).await?;
    let model = match &action {
        OllamaAction::Pull { model }
        | OllamaAction::Delete { model }
        | OllamaAction::Load { model }
        | OllamaAction::Unload { model } => model,
    };
    let _operation = app.adapters.reserve_model_operation(
        &config,
        model,
        matches!(&action, OllamaAction::Pull { .. }),
    )?;
    // Loading occupies GPU memory; pulls and deletes do not.
    let _workload = match &action {
        OllamaAction::Load { .. } => Some(app.adapters.text_workload(&config).await),
        _ => None,
    };
    let action = match action {
        OllamaAction::Pull { model } => crate::adapter_api::ModelAction::Pull { model },
        OllamaAction::Delete { model } => crate::adapter_api::ModelAction::Delete { model },
        OllamaAction::Load { model } => crate::adapter_api::ModelAction::Load {
            model,
            keep_alive: config.keep_alive.clone(),
        },
        OllamaAction::Unload { model } => crate::adapter_api::ModelAction::Unload { model },
    };
    Ok(app
        .adapters
        .management(&config)?
        .model_action(action)
        .await?)
}

pub(super) async fn search_hugging_face_models(
    app: &App,
    query: &str,
) -> Result<Vec<ModelSearchResult>> {
    Ok(app
        .adapters
        .catalogue()?
        .search(query)
        .await?
        .into_iter()
        .map(|result| ModelSearchResult {
            id: result.id,
            downloads: result.downloads,
            files: result.files,
        })
        .collect())
}

/// Download progress stays scoped to this request and connection. Dropping the
/// operation drops the upstream body and releases its concurrency slot.
pub(super) async fn download_model(
    app: &App,
    tx: &mpsc::Sender<Out>,
    request_id: u64,
    model: String,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    let work = async {
        let config = load_broker_config(&app.db).await?;
        let _operation = app
            .adapters
            .reserve_model_operation(&config, &model, true)?;
        let mut stream = app.adapters.management(&config)?.pull(model).await?;
        let mut last_sent = None::<Instant>;
        while let Some(update) = stream.next().await? {
            if let Some(progress) = update.download
                && (update.done
                    || last_sent.is_none_or(|last| last.elapsed() >= Duration::from_millis(250)))
            {
                let progress = chatty_protocol::admin::ModelDownloadProgress {
                    status: progress.status.chars().take(256).collect(),
                    digest: progress
                        .digest
                        .map(|digest| digest.chars().take(128).collect()),
                    total: progress.total,
                    completed: progress.completed,
                };
                tx.send((
                    MessageType::Response,
                    request_id,
                    encode(&Response::ModelDownloadProgress(progress))?.into(),
                ))
                .await?;
                last_sent = Some(Instant::now());
            }
            if update.done {
                return Ok::<_, Error>(());
            }
        }
        bail!("model download ended before reporting success")
    };
    let cancelled = tokio::select! {
        biased;
        _ = async {
            if !*cancel.borrow() { let _ = cancel.changed().await; }
        } => true,
        _ = tx.closed() => true,
        result = work => { result?; false },
    };
    tx.send((
        MessageType::Response,
        request_id,
        encode(&Response::ModelDownloadFinished { cancelled })?.into(),
    ))
    .await?;
    Ok(())
}
