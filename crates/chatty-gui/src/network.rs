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
    let mut remembered = if args.inspect {
        None
    } else {
        fs::read_to_string(&path)
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    let mut next_id = 1u64;
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
                    Some(Command::Request(request)) => { next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*request).await.is_err() { break; } }
                    Some(Command::SendThenGenerate { message, generate }) => { next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*message).await.is_err() { break; } next_id += 1; if write_message(&mut stream, MessageType::Request, next_id, &*generate).await.is_err() { break; } }
                    Some(Command::Cancel(id)) => if write_payload(&mut stream, MessageType::Cancel, id, vec![]).await.is_err() { break; },
                    Some(Command::Reconnect) => {
                        let _ = events.send(Event::Status("Reconnecting…".into()));
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        break;
                    }
                    Some(Command::ClearSession) => { remembered = None; let _ = fs::remove_file(&path); }
                    Some(Command::Stop) | None => return,
                },
                result = read_frame(&mut stream) => match result {
                    Ok(frame) => {
                        if frame.message_type == MessageType::Response { if let Ok(Response::Authenticated { session_token, .. }) = decode::<Response>(&frame.payload) { remembered = Some(session_token.clone()); if !args.inspect { save_session(&path, &session_token); } } }
                        if frame.message_type == MessageType::Error { if let Ok(error) = decode::<WireError>(&frame.payload) { if matches!(error.code, ErrorCode::Unauthorized) { remembered = None; let _ = fs::remove_file(&path); let _ = events.send(Event::SessionExpired); } } }
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

#[cfg(test)]
mod tests {
    use super::default_session_path;
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
}
