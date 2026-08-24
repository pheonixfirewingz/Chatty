use super::*;
use std::fs::{self, OpenOptions};
use std::io::Write as _;

pub(super) enum Command {
    Connect(ConnectionTarget),
    Disconnect,
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
    Connected { resuming_session: bool },
    ConnectionFailed(String),
    Disconnected,
    Frame(Frame),
    SessionExpired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectionTarget {
    pub broker: String,
    pub server_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SavedSession {
    pub broker: String,
    pub token: String,
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

fn save_session(path: &PathBuf, session: &SavedSession) {
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

pub(super) fn load_session(path: &PathBuf, legacy_broker: Option<&str>) -> Option<SavedSession> {
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

pub(super) fn preferences_path(session_path: &std::path::Path) -> PathBuf {
    session_path.with_extension("preferences")
}

pub(super) fn last_server_path(session_path: &std::path::Path) -> PathBuf {
    session_path.with_file_name("last-server")
}

pub(super) fn load_last_server(path: &std::path::Path) -> Option<String> {
    let server = fs::read_to_string(path).ok()?;
    let server = server.trim();
    (!server.is_empty()).then(|| server.to_owned())
}

fn save_last_server(path: &std::path::Path, server: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("{}\n", server.trim()));
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

async fn connect(args: &Args, target: &ConnectionTarget) -> Result<TlsStream<TcpStream>> {
    let ca_path = ca_path(args, target)?;
    let mut roots = RootCertStore::empty();
    let ca_file = File::open(&ca_path)
        .with_context(|| format!("could not open CA certificate {}", ca_path.display()))?;
    for cert in rustls_pemfile::certs(&mut BufReader::new(ca_file)) {
        roots.add(cert?).context("invalid pinned CA")?;
    }
    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(&target.broker).await?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(target.server_name.clone()).context("invalid server name")?;
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

fn ca_path(args: &Args, target: &ConnectionTarget) -> Result<PathBuf> {
    if let Some(path) = args.ca.clone() {
        return Ok(path);
    }

    let server_name = target.server_name.trim();
    if server_name.is_empty()
        || server_name == "."
        || server_name == ".."
        || server_name.contains(['/', '\\'])
    {
        anyhow::bail!("invalid server name for CA certificate lookup");
    }

    let server_ca = default_server_ca_path(
        server_name,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .context(
        "could not determine the Linux user config directory; set XDG_CONFIG_HOME, HOME, or CHATTY_CA",
    )?;

    // Preserve the repository-relative development default outside Flatpak.
    // A server-specific CA always wins when the user has installed one.
    if server_ca.is_file() || std::env::var_os("FLATPAK_ID").is_some() {
        Ok(server_ca)
    } else {
        Ok(PathBuf::from("certs/ca.pem"))
    }
}

fn default_server_ca_path(
    server_name: &str,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .map(|path| {
            path.join("chatty/server-cas")
                .join(format!("{server_name}.ca.pem"))
        })
}

pub(super) async fn run(
    args: Args,
    mut remembered: Option<SavedSession>,
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
    let mut target: Option<ConnectionTarget> = None;
    let mut established_for_target = false;
    let last_server = last_server_path(&path);
    loop {
        if target.is_none() {
            match commands.recv().await {
                Some(Command::Connect(requested)) => {
                    target = Some(requested);
                    established_for_target = false;
                }
                Some(Command::ClearSession) => {
                    remembered = None;
                    let _ = fs::remove_file(&path);
                    continue;
                }
                Some(Command::Stop) | None => return,
                _ => continue,
            }
        }

        let active_target = target.clone().expect("connection target is set");
        let _ = events.send(Event::Status("Connecting…".into()));
        let mut stream = match tokio::time::timeout(
            Duration::from_secs(8),
            connect(&args, &active_target),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                if established_for_target {
                    let _ = events.send(Event::Status(format!(
                        "{} · Offline: {error:#}",
                        current_utc_timestamp()
                    )));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                } else {
                    target = None;
                    let _ = events.send(Event::ConnectionFailed(format!(
                        "Could not connect: {error:#}"
                    )));
                }
                continue;
            }
            Err(_) => {
                if established_for_target {
                    let _ = events.send(Event::Status(format!(
                        "{} · Offline: connection timed out",
                        current_utc_timestamp()
                    )));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                } else {
                    target = None;
                    let _ = events.send(Event::ConnectionFailed(
                        "Could not connect: the server did not respond within 8 seconds".into(),
                    ));
                }
                continue;
            }
        };

        established_for_target = true;
        save_last_server(&last_server, &active_target.server_name);
        let resume_token = remembered
            .as_ref()
            .filter(|session| session.broker == active_target.broker)
            .map(|session| session.token.clone());
        let _ = events.send(Event::Connected {
            resuming_session: resume_token.is_some(),
        });
        let _ = events.send(Event::Status("Online · TLS 1.3".into()));
        next_id += 1;
        let _ = write_message(
            &mut stream,
            MessageType::Request,
            next_id,
            &Request::GetServerCapabilities,
        )
        .await;
        if let Some(token) = resume_token {
            next_id += 1;
            let _ = write_message(
                &mut stream,
                MessageType::Request,
                next_id,
                &Request::Resume {
                    session_token: token,
                    since_revision: 0,
                },
            )
            .await;
        }
        let mut reconnect = false;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Connect(_)) => {}
                    Some(Command::Disconnect) => {
                        target = None;
                        established_for_target = false;
                        let _ = events.send(Event::Disconnected);
                        break;
                    }
                    Some(Command::Request(request)) => {
                        next_id += 1;
                        if matches!(&*request, Request::Logout { .. }) {
                            remembered = None;
                            signed_out_through_request = Some(next_id);
                            let _ = fs::remove_file(&path);
                        }
                        if write_message(&mut stream, MessageType::Request, next_id, &*request).await.is_err() { reconnect = true; break; }
                    }
                    Some(Command::SendThenGenerate { message, generate }) => { next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*message).await.is_err() { reconnect = true; break; } next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*generate).await.is_err() { reconnect = true; break; } }
                    Some(Command::Cancel(id)) => if write_payload(&mut stream, MessageType::Cancel, id, vec![]).await.is_err() { reconnect = true; break; },
                    Some(Command::Reconnect) => {
                        let _ = events.send(Event::Status("Reconnecting…".into()));
                        reconnect = true;
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
                        if frame.message_type == MessageType::Response { if let Ok(Response::Authenticated { session_token, .. }) = decode::<Response>(&frame.payload) { let session = SavedSession { broker: active_target.broker.clone(), token: session_token.clone() }; remembered = Some(session.clone()); signed_out_through_request = None; if !args.inspect { save_session(&path, &session); } } }
                        if frame.message_type == MessageType::Error { if let Ok(error) = decode::<WireError>(&frame.payload) { if matches!(error.code, ErrorCode::Unauthorized) && remembered.as_ref().is_some_and(|session| session.broker == active_target.broker) { remembered = None; let _ = fs::remove_file(&path); let _ = events.send(Event::SessionExpired); } } }
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
                        reconnect = true;
                        break;
                    }
                }
            }
        }
        if reconnect && target.is_some() {
            tokio::time::sleep(Duration::from_secs(2)).await;
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
        SavedSession, default_server_ca_path, default_session_path,
        is_expected_post_logout_unauthorized, last_server_path, load_glass_mode, load_last_server,
        load_light_mode, load_session, load_transparency, save_last_server, save_preferences,
        save_session,
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

    #[test]
    fn saved_session_is_scoped_to_its_broker() {
        let path =
            std::env::temp_dir().join(format!("chatty-saved-session-{}", std::process::id()));
        let session = SavedSession {
            broker: "192.168.0.98:7443".into(),
            token: "secret-session-token".into(),
        };

        save_session(&path, &session);

        assert_eq!(load_session(&path, None), Some(session));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_session_requires_an_explicit_startup_broker() {
        let path =
            std::env::temp_dir().join(format!("chatty-legacy-session-{}", std::process::id()));
        std::fs::write(&path, "old-unscoped-token").unwrap();

        assert_eq!(load_session(&path, None), None);
        assert_eq!(
            load_session(&path, Some("192.168.0.98:7443")),
            Some(SavedSession {
                broker: "192.168.0.98:7443".into(),
                token: "old-unscoped-token".into(),
            })
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn server_ca_uses_absolute_xdg_config_home() {
        assert_eq!(
            default_server_ca_path(
                "192.168.0.98",
                Some(PathBuf::from("/tmp/xdg-config")),
                Some(PathBuf::from("/home/test")),
            ),
            Some(PathBuf::from(
                "/tmp/xdg-config/chatty/server-cas/192.168.0.98.ca.pem"
            )),
        );
    }

    #[test]
    fn server_ca_falls_back_to_home_config() {
        assert_eq!(
            default_server_ca_path(
                "broker.example.test",
                Some(PathBuf::from("relative-config")),
                Some(PathBuf::from("/home/test")),
            ),
            Some(PathBuf::from(
                "/home/test/.config/chatty/server-cas/broker.example.test.ca.pem"
            )),
        );
    }

    #[test]
    fn last_connected_server_round_trips() {
        let session =
            std::env::temp_dir().join(format!("chatty-last-server-session-{}", std::process::id()));
        let path = last_server_path(&session);

        save_last_server(&path, "  chatty.example.test  ");

        assert_eq!(
            load_last_server(&path).as_deref(),
            Some("chatty.example.test")
        );
        let _ = std::fs::remove_file(path);
    }
}
