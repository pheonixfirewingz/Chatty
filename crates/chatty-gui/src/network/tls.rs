//! TLS configuration, certificate trust, and the connect handshake.

use super::ConnectionTarget;
use crate::Args;
use chatty_protocol::util::{Context, Result, bail};
use chatty_protocol::{MessageType, read_frame};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingCertificate {
    pub target: ConnectionTarget,
    pub fingerprint: String,
    pub(super) certificate: Vec<u8>,
    trust_anchor: bool,
}

impl PendingCertificate {
    pub(super) fn from_chain(
        target: ConnectionTarget,
        certificates: &[CertificateDer<'_>],
    ) -> Result<Self> {
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

    pub(crate) fn is_ca(&self) -> bool {
        self.trust_anchor
    }
}

#[derive(Debug)]
pub(super) struct CertificateVerifier {
    pub(super) expected: Option<Vec<u8>>,
    pub(super) provider: Arc<rustls::crypto::CryptoProvider>,
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

pub(super) enum ConnectAttempt {
    Connected(Box<TlsStream<TcpStream>>),
    TrustRequired(PendingCertificate),
}

pub(super) fn ca_client_config(path: &Path) -> Result<Arc<ClientConfig>> {
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

pub(super) fn der_ca_client_config(path: &Path) -> Result<Arc<ClientConfig>> {
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

pub(super) fn public_client_config() -> Arc<ClientConfig> {
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

pub(super) fn custom_client_config(expected: Option<Vec<u8>>) -> Result<Arc<ClientConfig>> {
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

pub(super) fn pinned_client_config(certificate: Vec<u8>) -> Result<Arc<ClientConfig>> {
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

pub(super) async fn tls_connect(
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

pub(super) async fn verify_chatty_handshake(
    mut stream: TlsStream<TcpStream>,
) -> Result<TlsStream<TcpStream>> {
    let hello = read_frame(&mut stream).await?;
    let value: serde_json::Value = serde_json::from_slice(&hello.payload)?;
    if hello.message_type != MessageType::Handshake
        || value["protocol"] != 23
        || value["encoding"] != "bincode2"
    {
        bail!("unsupported broker handshake");
    }
    Ok(stream)
}

pub(super) async fn connect(args: &Args, target: &ConnectionTarget) -> Result<ConnectAttempt> {
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

pub(super) fn certificate_fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub(super) fn server_trust_path(target: &ConnectionTarget, extension: &str) -> Result<PathBuf> {
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

pub(super) fn save_trusted_certificate(pending: &PendingCertificate) -> Result<()> {
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

pub(super) fn default_server_trust_dir(
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
pub(super) fn default_server_ca_path(
    server_name: &str,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    default_server_trust_dir(xdg_config_home, home)
        .map(|path| path.join(format!("{server_name}.ca.pem")))
}
