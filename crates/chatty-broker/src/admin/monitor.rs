//! Broker health, resource monitor, and boot-log reader for administrators.
use super::*;

pub(crate) const MAX_ADMIN_LOG_RESPONSE_BYTES: u64 = 512 * 1024;

pub(crate) fn read_broker_log(path: &Path) -> Result<BrokerLog> {
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

pub(crate) async fn broker_monitor(app: &App) -> BrokerMonitor {
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

pub(crate) async fn adapter_health(app: &App) -> (AdapterStatus, u32, Option<u64>) {
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
