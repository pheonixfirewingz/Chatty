//! Boot log and per-user data paths.
use super::*;

const MAX_BOOT_LOG_FILE_BYTES: u64 = 16 * 1024 * 1024;

struct BootLogWriter {
    file: Arc<std::sync::Mutex<File>>,
}

impl Write for BootLogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        const ROLLOVER_MARKER: &[u8] = b"[broker log rolled over after reaching 16 MiB]\n";
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("boot log lock poisoned"))?;
        if file.metadata()?.len().saturating_add(buffer.len() as u64) > MAX_BOOT_LOG_FILE_BYTES {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(ROLLOVER_MARKER)?;
            let available = usize::try_from(
                MAX_BOOT_LOG_FILE_BYTES.saturating_sub(ROLLOVER_MARKER.len() as u64),
            )
            .unwrap_or(0);
            let kept = &buffer[buffer.len().saturating_sub(available)..];
            file.write_all(kept)?;
        } else {
            file.write_all(buffer)?;
        }
        drop(file);
        let _ = std::io::stderr().write_all(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .lock()
            .map_err(|_| io::Error::other("boot log lock poisoned"))?
            .flush()?;
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

pub(crate) fn open_boot_log(path: &Path) -> Result<File> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create broker log directory {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("create boot log {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure boot log {}", path.display()))?;
    }
    Ok(file)
}

pub(crate) fn init_boot_logging(path: &Path) -> Result<()> {
    let file = Arc::new(std::sync::Mutex::new(open_boot_log(path)?));
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("chatty_broker=trace".parse()?),
        )
        .with_writer(move || BootLogWriter {
            file: Arc::clone(&file),
        })
        .init();
    Ok(())
}

pub(crate) fn default_user_data_dir(
    xdg_data_home: Option<PathBuf>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_data_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            user_home
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".local/share"))
        })
        .map(|path| path.join("chatty"))
}

/// Creates and returns the user data directory; `override_env` names the
/// variable that bypasses it in the error message.
fn user_data_dir(override_env: &str) -> Result<PathBuf> {
    let data_dir = default_user_data_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .with_context(|| {
        format!(
            "could not determine the Linux user data directory; set XDG_DATA_HOME, HOME, or {override_env}"
        )
    })?;
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("create application data directory {}", data_dir.display()))?;
    Ok(data_dir)
}

pub(crate) fn default_database_url() -> Result<String> {
    let path = user_data_dir("CHATTY_DATABASE")?.join("chatty.db");
    Ok(format!("sqlite://{}?mode=rwc", path.display()))
}

pub(crate) fn default_broker_log_path() -> Result<PathBuf> {
    Ok(user_data_dir("CHATTY_LOG")?.join("chatty-boot.log"))
}
