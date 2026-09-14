use super::*;

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
    let response = app
        .http
        .get(format!("{}/models", config.adapter_url))
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    let Ok(response) = response.and_then(|response| response.error_for_status()) else {
        return (
            AdapterStatus::Offline,
            0,
            Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        );
    };
    let Ok(body) = response.json::<Value>().await else {
        return (
            AdapterStatus::Offline,
            0,
            Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        );
    };
    let models = body["data"].as_array().map_or(0, |models| models.len()) as u32;
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
    let row = sqlx::query("SELECT adapter_enabled,adapter_url,use_ollama_api,model,temperature,top_p,top_k,num_ctx,num_predict,repeat_penalty,seed,keep_alive,allow_public_characters,allow_self_registration FROM broker_settings WHERE singleton=1")
        .fetch_one(db)
        .await?;
    Ok(BrokerConfig {
        adapter_enabled: row.get("adapter_enabled"),
        adapter_url: row.get("adapter_url"),
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
    Ok(())
}

pub(super) fn ollama_base_url(adapter_url: &str) -> String {
    adapter_url
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or(adapter_url.trim_end_matches('/'))
        .to_owned()
}

pub(super) async fn ollama_url(app: &App) -> Result<String> {
    let config = load_broker_config(&app.db).await?;
    Ok(ollama_base_url(&config.adapter_url))
}

pub(super) fn required_model_name(model: String) -> Result<String> {
    let model = model.trim();
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        bail!("model name is invalid")
    }
    Ok(model.to_owned())
}

pub(super) async fn load_ollama_state(app: &App) -> Result<OllamaState> {
    let base = ollama_url(app).await?;
    let (version, tags, running) = tokio::try_join!(
        app.http.get(format!("{base}/api/version")).send(),
        app.http.get(format!("{base}/api/tags")).send(),
        app.http.get(format!("{base}/api/ps")).send(),
    )?;
    let version: Value = version.error_for_status()?.json().await?;
    let tags: Value = tags.error_for_status()?.json().await?;
    let running: Value = running.error_for_status()?.json().await?;
    let models = tags["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            Some(OllamaModel {
                name: model.get("name")?.as_str()?.to_owned(),
                size: model.get("size").and_then(Value::as_u64).unwrap_or(0),
                modified_at: model
                    .get("modified_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                family: model["details"]["family"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                parameter_size: model["details"]["parameter_size"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                quantization_level: model["details"]["quantization_level"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    let running_models = running["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            Some(OllamaRunningModel {
                name: model.get("name")?.as_str()?.to_owned(),
                size: model.get("size").and_then(Value::as_u64).unwrap_or(0),
                size_vram: model.get("size_vram").and_then(Value::as_u64).unwrap_or(0),
                expires_at: model
                    .get("expires_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    Ok(OllamaState {
        version: version["version"].as_str().unwrap_or("unknown").to_owned(),
        models,
        running_models,
    })
}

pub(super) async fn run_ollama_action(app: &App, action: OllamaAction) -> Result<()> {
    let base = ollama_url(app).await?;
    let response = match action {
        OllamaAction::Pull { model } => {
            app.http
                .post(format!("{base}/api/pull"))
                .json(&json!({"model": required_model_name(model)?, "stream": false}))
                .send()
                .await?
        }
        OllamaAction::Delete { model } => {
            app.http
                .delete(format!("{base}/api/delete"))
                .json(&json!({"model": required_model_name(model)?}))
                .send()
                .await?
        }
        OllamaAction::Load { model } => {
            let config = load_broker_config(&app.db).await?;
            app.http
                .post(format!("{base}/api/generate"))
                .json(&json!({
                    "model": required_model_name(model)?,
                    "keep_alive": config.keep_alive,
                    "stream": false
                }))
                .send()
                .await?
        }
        OllamaAction::Unload { model } => {
            app.http
                .post(format!("{base}/api/generate"))
                .json(&json!({
                    "model": required_model_name(model)?,
                    "keep_alive": 0,
                    "stream": false
                }))
                .send()
                .await?
        }
    };
    response.error_for_status()?;
    Ok(())
}

pub(super) async fn adapter_url(app: &App) -> Result<String> {
    let config = load_broker_config(&app.db).await?;
    if !config.adapter_enabled {
        bail!("inference adapter is disabled")
    }
    Ok(config.adapter_url)
}
