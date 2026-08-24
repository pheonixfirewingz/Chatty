use super::*;
use std::fs::{self, OpenOptions};
use std::io::Write as _;

pub(super) enum Command {
    Request(Box<Request>),
    SendThenGenerate {
        message: Box<Request>,
        generate: Box<Request>,
    },
    Cancel(u64),
    Reconnect,
    ClearSession,
    Stop,
}

pub(super) enum Event {
    Status(String),
    Frame(Frame),
    SessionExpired,
}

pub(super) fn session_path(args: &Args) -> Result<PathBuf> {
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

fn default_session_path(xdg_state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
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

fn save_session(path: &PathBuf, token: &str) {
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
        let _ = file.write_all(token.as_bytes());
    }
}

pub(super) fn load_session(path: &PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .filter(|token| !token.trim().is_empty())
}

pub(super) fn preferences_path(session_path: &std::path::Path) -> PathBuf {
    session_path.with_extension("preferences")
}

pub(super) fn load_light_mode(path: &std::path::Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|value| value.lines().any(|line| line.trim() == "theme=light"))
}

pub(super) fn load_glass_mode(path: &std::path::Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|value| value.lines().any(|line| line.trim() == "surface=glass"))
}

pub(super) fn load_transparency(path: &std::path::Path) -> u8 {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| {
            value.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("transparency=")?
                    .parse::<u8>()
                    .ok()
            })
        })
        .unwrap_or(20)
        .min(80)
}

pub(super) fn save_preferences(
    path: &std::path::Path,
    light_mode: bool,
    glass_mode: bool,
    transparency: u8,
) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let theme = if light_mode { "light" } else { "dark" };
    let surface = if glass_mode { "glass" } else { "solid" };
    let transparency = transparency.min(80);
    let _ = fs::write(
        path,
        format!("theme={theme}\nsurface={surface}\ntransparency={transparency}\n"),
    );
}

async fn connect(args: &Args) -> Result<TlsStream<TcpStream>> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut BufReader::new(File::open(&args.ca)?)) {
        roots.add(cert?).context("invalid pinned CA")?;
    }
    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(&args.broker).await?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(args.server_name.clone()).context("invalid server name")?;
    let mut stream = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .context("TLS certificate verification failed")?;
    let hello = read_frame(&mut stream).await?;
    let value: serde_json::Value = serde_json::from_slice(&hello.payload)?;
    if hello.message_type != MessageType::Handshake
        || value["protocol"] != 9
        || value["encoding"] != "bincode2"
    {
        anyhow::bail!("unsupported broker handshake");
    }
    Ok(stream)
}

pub(super) async fn run(
    args: Args,
    mut remembered: Option<String>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: std::sync::mpsc::Sender<Event>,
) {
    let path = match session_path(&args) {
        Ok(path) => path,
        Err(error) => {
            let _ = events.send(Event::Status(format!(
                "{} · Startup failed: {error:#}",
                current_utc_timestamp()
            )));
            return;
        }
    };
    let mut next_id = 1u64;
    let mut signed_out_through_request = None;
    loop {
        let _ = events.send(Event::Status("Connecting…".into()));
        let mut stream = match connect(&args).await {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(Event::Status(format!(
                    "{} · Offline: {e}",
                    current_utc_timestamp()
                )));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let _ = events.send(Event::Status("Online · TLS 1.3".into()));
        next_id += 1;
        let _ = write_message(
            &mut stream,
            MessageType::Request,
            next_id,
            &Request::GetServerCapabilities,
        )
        .await;
        if let Some(token) = remembered.clone() {
            next_id += 1;
            let _ = write_message(
                &mut stream,
                MessageType::Request,
                next_id,
                &Request::Resume {
                    session_token: token.clone(),
                    since_revision: 0,
                },
            )
            .await;
        }
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Request(request)) => {
                        next_id += 1;
                        if matches!(&*request, Request::Logout { .. }) {
                            remembered = None;
                            signed_out_through_request = Some(next_id);
                            let _ = fs::remove_file(&path);
                        }
                        if write_message(&mut stream, MessageType::Request, next_id, &*request).await.is_err() { break; }
                    }
                    Some(Command::SendThenGenerate { message, generate }) => { next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*message).await.is_err() { break; } next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*generate).await.is_err() { break; } }
                    Some(Command::Cancel(id)) => if write_payload(&mut stream, MessageType::Cancel, id, vec![]).await.is_err() { break; },
                    Some(Command::Reconnect) => {
                        let _ = events.send(Event::Status("Reconnecting…".into()));
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        break;
                    }
                    Some(Command::ClearSession) => {
                        remembered = None;
                        signed_out_through_request = Some(next_id);
                        let _ = fs::remove_file(&path);
                    }
                    Some(Command::Stop) | None => return,
                },
                result = read_frame(&mut stream) => match result {
                    Ok(frame) => {
                        if frame.message_type == MessageType::Response { if let Ok(Response::Authenticated { session_token, .. }) = decode::<Response>(&frame.payload) { remembered = Some(session_token.clone()); signed_out_through_request = None; if !args.inspect { save_session(&path, &session_token); } } }
                        if frame.message_type == MessageType::Error { if let Ok(error) = decode::<WireError>(&frame.payload) { if matches!(error.code, ErrorCode::Unauthorized) && remembered.is_some() { remembered = None; let _ = fs::remove_file(&path); let _ = events.send(Event::SessionExpired); } } }
                        if is_expected_post_logout_unauthorized(&frame, signed_out_through_request) {
                            continue;
                        }
                        if events.send(Event::Frame(frame)).is_err() { return; }
                    }
                    Err(e) => {
                        let _ = events.send(Event::Status(format!(
                            "{} · Offline: {e}",
                            current_utc_timestamp()
                        )));
                        break;
                    }
                }
            }
        }
    }
}

fn is_expected_post_logout_unauthorized(frame: &Frame, cutoff: Option<u64>) -> bool {
    frame.message_type == MessageType::Error
        && cutoff.is_some_and(|request_id| frame.request_id <= request_id)
        && decode::<WireError>(&frame.payload)
            .is_ok_and(|error| matches!(error.code, ErrorCode::Unauthorized))
}

#[cfg(test)]
mod tests {
    use super::{
        default_session_path, is_expected_post_logout_unauthorized, load_glass_mode,
        load_light_mode, load_transparency, save_preferences,
    };
    use chatty_protocol::{ErrorCode, Frame, MessageType, WireError, encode};
    use std::path::PathBuf;

    #[test]
    fn session_uses_absolute_xdg_state_home() {
        assert_eq!(
            default_session_path(
                Some(PathBuf::from("/tmp/xdg-state")),
                Some(PathBuf::from("/home/tester")),
            ),
            Some(PathBuf::from("/tmp/xdg-state/chatty/session")),
        );
    }

    #[test]
    fn session_uses_linux_home_fallback() {
        assert_eq!(
            default_session_path(None, Some(PathBuf::from("/home/tester"))),
            Some(PathBuf::from("/home/tester/.local/state/chatty/session",)),
        );
    }

    #[test]
    fn relative_xdg_state_home_is_ignored() {
        assert_eq!(
            default_session_path(
                Some(PathBuf::from("relative/state")),
                Some(PathBuf::from("/home/tester")),
            ),
            Some(PathBuf::from("/home/tester/.local/state/chatty/session",)),
        );
    }

    #[test]
    fn session_never_falls_back_to_current_directory() {
        assert_eq!(default_session_path(None, None), None);
        assert_eq!(
            default_session_path(
                Some(PathBuf::from("relative/state")),
                Some(PathBuf::from("relative/home")),
            ),
            None,
        );
    }

    #[test]
    fn unauthorized_from_pre_logout_request_is_expected() {
        let frame = Frame {
            compressed: false,
            message_type: MessageType::Error,
            request_id: 12,
            payload: encode(&WireError {
                code: ErrorCode::Unauthorized,
                message: "unauthorized".into(),
                retryable: false,
            })
            .unwrap(),
        };

        assert!(is_expected_post_logout_unauthorized(&frame, Some(12)));
        assert!(!is_expected_post_logout_unauthorized(&frame, Some(11)));
        assert!(!is_expected_post_logout_unauthorized(&frame, None));
    }

    #[test]
    fn appearance_preferences_round_trip_together() {
        let path = std::env::temp_dir().join(format!(
            "chatty-appearance-preferences-{}",
            std::process::id()
        ));

        save_preferences(&path, true, true, 80);
        assert!(load_light_mode(&path));
        assert!(load_glass_mode(&path));
        assert_eq!(load_transparency(&path), 80);

        save_preferences(&path, false, false, 25);
        assert!(!load_light_mode(&path));
        assert!(!load_glass_mode(&path));
        assert_eq!(load_transparency(&path), 25);
        let _ = std::fs::remove_file(path);
    }
}
