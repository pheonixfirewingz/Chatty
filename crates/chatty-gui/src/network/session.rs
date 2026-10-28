//! Saved session, last server, and preferences on disk.

use crate::Args;
use chatty_protocol::util::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedSession {
    pub broker: String,
    pub token: String,
}

pub(crate) fn session_path(args: &Args) -> Result<PathBuf> {
    if let Some(path) = args.session_file.clone() {
        return Ok(path);
    }

    default_session_path(
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .context(
        "could not determine the Linux user state directory; set XDG_STATE_HOME, HOME, or CHATTY_SESSION_FILE",
    )
}

pub(super) fn default_session_path(
    xdg_state_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    // The XDG specification requires values to be absolute. Ignore a relative
    // XDG value and use its documented HOME fallback instead.
    xdg_state_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".local/state"))
        })
        .map(|path| path.join("chatty/session"))
}

pub(super) fn save_session(path: &PathBuf, session: &SavedSession) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path);
    if let Ok(mut file) = file {
        let value = serde_json::json!({
            "broker": session.broker,
            "token": session.token,
        });
        let _ = file.write_all(value.to_string().as_bytes());
    }
}

pub(crate) fn load_session(path: &PathBuf, legacy_broker: Option<&str>) -> Option<SavedSession> {
    let contents = fs::read_to_string(path).ok()?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
        let broker = value.get("broker")?.as_str()?.trim();
        let token = value.get("token")?.as_str()?.trim();
        if broker.is_empty() || token.is_empty() {
            return None;
        }
        return Some(SavedSession {
            broker: broker.into(),
            token: token.into(),
        });
    }

    let broker = legacy_broker?.trim();
    let token = contents.trim();
    if broker.is_empty() || token.is_empty() {
        return None;
    }
    // Old Chatty versions stored only the token. It is safe to migrate when
    // startup configuration also identifies the old broker explicitly.
    Some(SavedSession {
        broker: broker.into(),
        token: token.into(),
    })
}

pub(crate) fn preferences_path(session_path: &Path) -> PathBuf {
    session_path.with_extension("preferences")
}

pub(crate) fn last_server_path(session_path: &Path) -> PathBuf {
    session_path.with_file_name("last-server")
}

pub(crate) fn load_last_server(path: &Path) -> Option<String> {
    let server = fs::read_to_string(path).ok()?;
    let server = server.trim();
    (!server.is_empty()).then(|| server.to_owned())
}

pub(super) fn save_last_server(path: &Path, server: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        path,
        format!(
            "{server}
"
        ),
    );
}

pub(crate) fn load_tts_auto_speak(path: &Path) -> bool {
    fs::read_to_string(path).ok().is_some_and(|value| {
        value
            .lines()
            .any(|line| line.trim() == "tts_auto_speak=true")
    })
}

pub(crate) fn save_preferences(path: &Path, tts_auto_speak: bool) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("tts_auto_speak={tts_auto_speak}\n"));
}
