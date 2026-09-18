use super::*;
use rustls::{
    DigitallySignedStruct, Error as TlsError, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, UnixTime},
};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write as _;

pub(super) enum Command {
    Connect(ConnectionTarget),
    TrustCertificate(PendingCertificate),
    RejectCertificate,
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
    CertificateTrustRequired(PendingCertificate),
    Disconnected,
    Frame(Frame),
    SessionExpired,
}

#[derive(Clone)]
pub(super) struct EventSender {
    events: std::sync::mpsc::Sender<Event>,
    repaint_context: Arc<std::sync::Mutex<Option<egui::Context>>>,
}

impl EventSender {
    pub(super) fn new(
        events: std::sync::mpsc::Sender<Event>,
        repaint_context: Arc<std::sync::Mutex<Option<egui::Context>>>,
    ) -> Self {
        Self {
            events,
            repaint_context,
        }
    }

    pub(super) fn send(
        &self,
        event: Event,
    ) -> std::result::Result<(), std::sync::mpsc::SendError<Event>> {
        let result = self.events.send(event);
        if result.is_ok()
            && let Ok(context) = self.repaint_context.lock()
            && let Some(context) = context.as_ref()
        {
            context.request_repaint();
        }
        result
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectionTarget {
    pub broker: String,
    pub server_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingCertificate {
    pub target: ConnectionTarget,
    pub fingerprint: String,
    certificate: Vec<u8>,
    trust_anchor: bool,
}

impl PendingCertificate {
    #[cfg(test)]
    pub(super) fn from_der(target: ConnectionTarget, certificate: Vec<u8>) -> Self {
        let fingerprint = certificate_fingerprint(&certificate);
        Self {
            target,
            fingerprint,
            certificate,
            trust_anchor: false,
        }
    }

    fn from_chain(target: ConnectionTarget, certificates: &[CertificateDer<'_>]) -> Result<Self> {
        let trust_anchor = certificates.len() > 1;
        let certificate = certificates
            .last()
            .context("broker did not present a certificate")?
            .as_ref()
            .to_vec();
        let fingerprint = certificate_fingerprint(&certificate);
        Ok(Self {
            target,
            fingerprint,
            certificate,
            trust_anchor,
        })
    }

    pub(super) fn is_ca(&self) -> bool {
        self.trust_anchor
    }
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

pub(super) fn load_tts_auto_speak(path: &std::path::Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|value| value.lines().any(|line| line.trim() == "tts_auto_speak=true"))
}

pub(super) fn save_preferences(
    path: &std::path::Path,
    light_mode: bool,
    glass_mode: bool,
    transparency: u8,
    tts_auto_speak: bool,
) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let theme = if light_mode { "light" } else { "dark" };
    let surface = if glass_mode { "glass" } else { "solid" };
    let transparency = transparency.min(80);
    let _ = fs::write(
        path,
        format!(
            "theme={theme}\nsurface={surface}\ntransparency={transparency}\ntts_auto_speak={tts_auto_speak}\n"
        ),
    );
}

#[derive(Debug)]
struct CertificateVerifier {
    expected: Option<Vec<u8>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for CertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        if self
            .expected
            .as_ref()
            .is_none_or(|expected| expected.as_slice() == end_entity.as_ref())
        {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(
                "the broker certificate no longer matches the trusted certificate".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

enum ConnectAttempt {
    Connected(Box<TlsStream<TcpStream>>),
    TrustRequired(PendingCertificate),
}

fn ca_client_config(path: &std::path::Path) -> Result<Arc<ClientConfig>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<ClientConfig>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(config) = cache.lock().unwrap().get(path) {
        return Ok(config.clone());
    }
    let mut roots = RootCertStore::empty();
    let ca_file = File::open(path)
        .with_context(|| format!("could not open CA certificate {}", path.display()))?;
    for cert in chatty_protocol::util::pemfile::certs(&mut BufReader::new(ca_file)) {
        roots.add(cert?).context("invalid pinned CA")?;
    }
    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    let config = Arc::new(config);
    cache
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), config.clone());
    Ok(config)
}

fn der_ca_client_config(path: &std::path::Path) -> Result<Arc<ClientConfig>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<ClientConfig>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(config) = cache.lock().unwrap().get(path) {
        return Ok(config.clone());
    }
    let certificate = fs::read(path)
        .with_context(|| format!("could not read trusted CA certificate {}", path.display()))?;
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate))
        .context("invalid trusted CA certificate")?;
    let config = Arc::new(
        ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    cache
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), config.clone());
    Ok(config)
}

fn public_client_config() -> Arc<ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(
                ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

fn custom_client_config(expected: Option<Vec<u8>>) -> Result<Arc<ClientConfig>> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .context("TLS crypto provider is not installed")?;
    let verifier = Arc::new(CertificateVerifier { expected, provider });
    Ok(Arc::new(
        ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    ))
}

fn pinned_client_config(certificate: Vec<u8>) -> Result<Arc<ClientConfig>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<u8>, Arc<ClientConfig>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(config) = cache.lock().unwrap().get(&certificate) {
        return Ok(config.clone());
    }
    let config = custom_client_config(Some(certificate.clone()))?;
    cache.lock().unwrap().insert(certificate, config.clone());
    Ok(config)
}

async fn tls_connect(
    config: Arc<ClientConfig>,
    target: &ConnectionTarget,
) -> Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(&target.broker).await?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(target.server_name.clone()).context("invalid server name")?;
    TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .context("TLS certificate verification failed")
}

async fn verify_chatty_handshake(mut stream: TlsStream<TcpStream>) -> Result<TlsStream<TcpStream>> {
    let hello = read_frame(&mut stream).await?;
    let value: serde_json::Value = serde_json::from_slice(&hello.payload)?;
    if hello.message_type != MessageType::Handshake
        || value["protocol"] != 12
        || value["encoding"] != "bincode2"
    {
        bail!("unsupported broker handshake");
    }
    Ok(stream)
}

async fn connect(args: &Args, target: &ConnectionTarget) -> Result<ConnectAttempt> {
    if let Some(path) = args.ca.clone() {
        return verify_chatty_handshake(tls_connect(ca_client_config(&path)?, target).await?)
            .await
            .map(|stream| ConnectAttempt::Connected(Box::new(stream)));
    }

    let ca_path = server_trust_path(target, "ca.pem")?;
    if ca_path.is_file() {
        return verify_chatty_handshake(tls_connect(ca_client_config(&ca_path)?, target).await?)
            .await
            .map(|stream| ConnectAttempt::Connected(Box::new(stream)));
    }

    let der_ca_path = server_trust_path(target, "ca.der")?;
    if der_ca_path.is_file() {
        return verify_chatty_handshake(
            tls_connect(der_ca_client_config(&der_ca_path)?, target).await?,
        )
        .await
        .map(|stream| ConnectAttempt::Connected(Box::new(stream)));
    }

    let certificate_path = server_trust_path(target, "cert.der")?;
    if certificate_path.is_file() {
        let certificate = fs::read(&certificate_path).with_context(|| {
            format!(
                "could not read trusted certificate {}",
                certificate_path.display()
            )
        })?;
        return verify_chatty_handshake(
            tls_connect(pinned_client_config(certificate)?, target).await?,
        )
        .await
        .map(|stream| ConnectAttempt::Connected(Box::new(stream)));
    }

    match tls_connect(public_client_config(), target).await {
        Ok(stream) => verify_chatty_handshake(stream)
            .await
            .map(|stream| ConnectAttempt::Connected(Box::new(stream))),
        Err(public_error) => {
            let stream = tls_connect(custom_client_config(None)?, target)
                .await
                .with_context(|| {
                    format!("public certificate validation failed: {public_error:#}")
                })?;
            let certificates = stream
                .get_ref()
                .1
                .peer_certificates()
                .context("broker did not present a certificate")?;
            Ok(ConnectAttempt::TrustRequired(
                PendingCertificate::from_chain(target.clone(), certificates)?,
            ))
        }
    }
}

fn certificate_fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn server_trust_path(target: &ConnectionTarget, extension: &str) -> Result<PathBuf> {
    let server_name = target.server_name.trim();
    if server_name.is_empty()
        || server_name == "."
        || server_name == ".."
        || server_name.contains(['/', '\\'])
    {
        bail!("invalid server name for certificate lookup");
    }
    default_server_trust_dir(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .map(|directory| directory.join(format!("{server_name}.{extension}")))
    .context("could not determine the Linux user config directory; set XDG_CONFIG_HOME or HOME")
}

fn save_trusted_certificate(pending: &PendingCertificate) -> Result<()> {
    let extension = if pending.trust_anchor {
        "ca.der"
    } else {
        "cert.der"
    };
    let path = server_trust_path(&pending.target, extension)?;
    let parent = path
        .parent()
        .context("trusted certificate path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "could not create certificate directory {}",
            parent.display()
        )
    })?;
    fs::write(&path, &pending.certificate)
        .with_context(|| format!("could not save trusted certificate {}", path.display()))
}

fn default_server_trust_dir(
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .map(|path| path.join("chatty/server-cas"))
}

#[cfg(test)]
fn default_server_ca_path(
    server_name: &str,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    default_server_trust_dir(xdg_config_home, home)
        .map(|path| path.join(format!("{server_name}.ca.pem")))
}

/// Favors queued commands over the live channel so nothing typed while the
/// link is down is lost.
async fn next_command(
    pending: &mut VecDeque<Command>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> Option<Command> {
    if let Some(command) = pending.pop_front() {
        return Some(command);
    }
    commands.recv().await
}

/// Waits for user action instead of holding an idle wire. Returns `None`
/// only when the app asked to stop.
async fn park_for_command(
    pending: &mut VecDeque<Command>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> Option<()> {
    match commands.recv().await {
        Some(Command::Stop) | None => None,
        Some(command) => {
            pending.push_back(command);
            Some(())
        }
    }
}

pub(super) async fn run(
    args: Args,
    mut remembered: Option<SavedSession>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: EventSender,
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
    let mut pending = VecDeque::<Command>::new();
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
            Ok(Ok(ConnectAttempt::Connected(stream))) => *stream,
            Ok(Ok(ConnectAttempt::TrustRequired(pending_certificate))) => {
                let _ = events.send(Event::CertificateTrustRequired(pending_certificate.clone()));
                match commands.recv().await {
                    Some(Command::TrustCertificate(accepted))
                        if accepted == pending_certificate =>
                    {
                        if let Err(error) = save_trusted_certificate(&accepted) {
                            target = None;
                            let _ = events.send(Event::ConnectionFailed(format!(
                                "Could not save trusted certificate: {error:#}"
                            )));
                        }
                    }
                    Some(Command::Connect(requested)) => {
                        target = Some(requested);
                    }
                    Some(Command::Stop) | None => return,
                    Some(Command::RejectCertificate) | Some(Command::Disconnect) => {
                        target = None;
                        let _ = events.send(Event::Disconnected);
                    }
                    _ => {
                        target = None;
                        let _ = events.send(Event::ConnectionFailed(
                            "The broker certificate was not trusted.".into(),
                        ));
                    }
                }
                continue;
            }
            Ok(Err(error)) => {
                if established_for_target {
                    let _ = events.send(Event::Status(format!(
                        "{} · Offline: {error:#}",
                        current_utc_timestamp()
                    )));
                    if park_for_command(&mut pending, &mut commands)
                        .await
                        .is_none()
                    {
                        return;
                    }
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
                    if park_for_command(&mut pending, &mut commands)
                        .await
                        .is_none()
                    {
                        return;
                    }
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
        // Persistent zstd contexts + scratch buffers for this connection.
        let mut codec = match ProtocolCodec::new() {
            Ok(codec) => codec,
            Err(error) => {
                let _ = events.send(Event::ConnectionFailed(format!(
                    "Could not connect: {error}"
                )));
                target = None;
                continue;
            }
        };
        let resume_token = remembered
            .as_ref()
            .filter(|session| session.broker == active_target.broker)
            .map(|session| session.token.clone());
        let _ = events.send(Event::Connected {
            resuming_session: resume_token.is_some(),
        });
        let _ = events.send(Event::Status("Online · TLS 1.3".into()));
        next_id += 1;
        let _ = codec
            .write_message(
                &mut stream,
                MessageType::Request,
                next_id,
                &Request::GetServerCapabilities,
            )
            .await;
        if let Some(token) = resume_token {
            next_id += 1;
            let _ = codec
                .write_message(
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
        let mut pending_generations = HashMap::<u64, Box<Request>>::new();
        loop {
            tokio::select! {
                command = next_command(&mut pending, &mut commands) => match command {
                    Some(Command::Connect(_)) => {}
                    Some(Command::TrustCertificate(_)) | Some(Command::RejectCertificate) => {}
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
                        if codec.write_message(&mut stream, MessageType::Request, next_id, &*request).await.is_err() { reconnect = true; break; }
                    }
                    Some(Command::SendThenGenerate { message, generate }) => {
                        next_id += 1;
                        let message_request_id = next_id;
                        if codec.write_message(&mut stream, MessageType::Request, message_request_id, &*message).await.is_err() {
                            reconnect = true;
                            break;
                        }
                        pending_generations.insert(message_request_id, generate);
                    }
                    Some(Command::Cancel(id)) => if codec.write_payload(&mut stream, MessageType::Cancel, id, &[]).await.is_err() { reconnect = true; break; },
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
                result = codec.read_frame(&mut stream) => match result {
                    Ok(frame) => {
                        // Decode each frame at most once in this loop.
                        let response = (frame.message_type == MessageType::Response)
                            .then(|| decode::<Response>(&frame.payload).ok())
                            .flatten();
                        let error = (frame.message_type == MessageType::Error)
                            .then(|| decode::<WireError>(&frame.payload).ok())
                            .flatten();
                        let pending_generation = take_accepted_generation(
                            response.as_ref(),
                            frame.request_id,
                            frame.message_type == MessageType::Error,
                            &mut pending_generations,
                        );
                        if let Some(Response::Authenticated { session_token, .. }) = response { let session = SavedSession { broker: active_target.broker.clone(), token: session_token }; remembered = Some(session.clone()); signed_out_through_request = None; if !args.inspect { save_session(&path, &session); } }
                        if let Some(error) = &error { if matches!(error.code, ErrorCode::Unauthorized) && remembered.as_ref().is_some_and(|session| session.broker == active_target.broker) { remembered = None; let _ = fs::remove_file(&path); let _ = events.send(Event::SessionExpired); } }
                        if is_expected_post_logout_unauthorized(frame.request_id, error.as_ref(), signed_out_through_request) {
                            continue;
                        }
                        if events.send(Event::Frame(frame)).is_err() { return; }
                        if let Some(generate) = pending_generation {
                            next_id += 1;
                            if codec.write_message(&mut stream, MessageType::Request, next_id, &*generate).await.is_err() {
                                reconnect = true;
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let idle_close = matches!(
                            &e,
                            chatty_protocol::ProtocolError::Io(io)
                                if io.kind() == std::io::ErrorKind::UnexpectedEof
                        );
                        let state = if idle_close { "Idle · offline" } else { "Offline" };
                        let _ = events.send(Event::Status(format!(
                            "{} · {state}: {e}",
                            current_utc_timestamp()
                        )));
                        reconnect = true;
                        break;
                    }
                }
            }
        }
        if reconnect && target.is_some() {
            // The broker closes quiet connections; go on-demand until the
            // client acts again.
            if park_for_command(&mut pending, &mut commands)
                .await
                .is_none()
            {
                return;
            }
        }
    }
}

fn take_accepted_generation(
    response: Option<&Response>,
    request_id: u64,
    is_error_frame: bool,
    pending: &mut HashMap<u64, Box<Request>>,
) -> Option<Box<Request>> {
    match response {
        Some(Response::Accepted { .. }) => pending.remove(&request_id),
        _ if is_error_frame => {
            pending.remove(&request_id);
            None
        }
        _ => None,
    }
}

fn is_expected_post_logout_unauthorized(
    request_id: u64,
    error: Option<&WireError>,
    cutoff: Option<u64>,
) -> bool {
    cutoff.is_some_and(|cutoff| request_id <= cutoff)
        && error.is_some_and(|error| matches!(error.code, ErrorCode::Unauthorized))
}

#[cfg(test)]
mod tests {
    use super::{
        CertificateVerifier, Event, EventSender, PendingCertificate, SavedSession,
        certificate_fingerprint, default_server_ca_path, default_session_path,
        is_expected_post_logout_unauthorized, last_server_path, load_glass_mode, load_last_server,
        load_light_mode, load_session, load_transparency, load_tts_auto_speak, save_last_server,
        save_preferences, save_session, take_accepted_generation,
    };
    use chatty_protocol::{ErrorCode, Request, Response, WireError};
    use eframe::egui;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[test]
    fn generation_is_released_only_after_its_user_message_is_accepted() {
        let mut pending = HashMap::new();
        pending.insert(
            21,
            Box::new(Request::Generate {
                session_token: "session".into(),
                conversation_id: "conversation".into(),
                speaker_id: Some("character".into()),
                parent_id: None,
            }),
        );
        let other_accepted = Response::Accepted {
            entity_id: Some("other".into()),
            revision: 1,
        };
        assert!(take_accepted_generation(Some(&other_accepted), 20, false, &mut pending).is_none());

        let accepted = Response::Accepted {
            entity_id: Some("message".into()),
            revision: 2,
        };
        assert!(matches!(
            take_accepted_generation(
                Some(&accepted),
                21,
                false,
                &mut pending
            ),
            Some(request) if matches!(*request, Request::Generate { .. })
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn network_events_wake_the_egui_event_loop() {
        let context = egui::Context::default();
        let repaint_count = Arc::new(AtomicUsize::new(0));
        let callback_count = repaint_count.clone();
        context.set_request_repaint_callback(move |_| {
            callback_count.fetch_add(1, Ordering::SeqCst);
        });
        let repaint_context = Arc::new(Mutex::new(Some(context)));
        let (events, received) = std::sync::mpsc::channel();
        let sender = EventSender::new(events, repaint_context);
        let before = repaint_count.load(Ordering::SeqCst);

        sender.send(Event::Status("Online".into())).unwrap();

        assert!(matches!(received.recv().unwrap(), Event::Status(status) if status == "Online"));
        assert!(repaint_count.load(Ordering::SeqCst) > before);
    }

    #[test]
    fn certificate_fingerprint_is_colon_separated_sha256() {
        assert_eq!(
            certificate_fingerprint(b""),
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
    }

    #[test]
    fn pinned_verifier_rejects_a_changed_certificate() {
        let verifier = CertificateVerifier {
            expected: Some(vec![1, 2, 3]),
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        };
        let expected = rustls::pki_types::CertificateDer::from(vec![1, 2, 3]);
        let changed = rustls::pki_types::CertificateDer::from(vec![1, 2, 4]);
        let name = rustls::pki_types::ServerName::try_from("private.example".to_owned()).unwrap();
        let now = rustls::pki_types::UnixTime::since_unix_epoch(Duration::from_secs(0));

        assert!(
            rustls::client::danger::ServerCertVerifier::verify_server_cert(
                &verifier,
                &expected,
                &[],
                &name,
                &[],
                now,
            )
            .is_ok()
        );
        assert!(
            rustls::client::danger::ServerCertVerifier::verify_server_cert(
                &verifier,
                &changed,
                &[],
                &name,
                &[],
                now,
            )
            .is_err()
        );
    }

    #[test]
    fn enrollment_uses_the_last_certificate_as_a_private_ca() {
        let chain = [
            rustls::pki_types::CertificateDer::from(vec![1, 2, 3]),
            rustls::pki_types::CertificateDer::from(vec![4, 5, 6]),
        ];
        let pending = PendingCertificate::from_chain(
            super::ConnectionTarget {
                broker: "private.example:7443".into(),
                server_name: "private.example".into(),
            },
            &chain,
        )
        .unwrap();

        assert!(pending.is_ca());
        assert_eq!(pending.certificate, vec![4, 5, 6]);
        assert_eq!(pending.fingerprint, certificate_fingerprint(&[4, 5, 6]));
    }

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
        let error = WireError {
            code: ErrorCode::Unauthorized,
            message: "unauthorized".into(),
            retryable: false,
        };

        assert!(is_expected_post_logout_unauthorized(
            12,
            Some(&error),
            Some(12)
        ));
        assert!(!is_expected_post_logout_unauthorized(
            12,
            Some(&error),
            Some(11)
        ));
        assert!(!is_expected_post_logout_unauthorized(
            12,
            Some(&error),
            None
        ));
        assert!(!is_expected_post_logout_unauthorized(12, None, Some(12)));
    }

    #[test]
    fn appearance_preferences_round_trip_together() {
        let path = std::env::temp_dir().join(format!(
            "chatty-appearance-preferences-{}",
            std::process::id()
        ));

        save_preferences(&path, true, true, 80, true);
        assert!(load_light_mode(&path));
        assert!(load_glass_mode(&path));
        assert_eq!(load_transparency(&path), 80);
        assert!(load_tts_auto_speak(&path));

        save_preferences(&path, false, false, 25, false);
        assert!(!load_light_mode(&path));
        assert!(!load_glass_mode(&path));
        assert_eq!(load_transparency(&path), 25);
        assert!(!load_tts_auto_speak(&path));
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
