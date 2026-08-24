use anyhow::{Context, Result, bail};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use chatty_protocol::*;
use clap::Parser;
use futures_util::{StreamExt, TryStreamExt};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, broadcast, mpsc, watch},
    time::Instant,
};
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Parser, Clone)]
struct Args {
    #[arg(long, env = "CHATTY_LISTEN", default_value = "0.0.0.0:7443")]
    listen: String,
    #[arg(long, env = "CHATTY_DATABASE")]
    database: Option<String>,
    #[arg(long, env = "CHATTY_CERT", default_value = "certs/server.pem")]
    cert: String,
    #[arg(long, env = "CHATTY_KEY", default_value = "certs/server.key")]
    key: String,
    #[arg(
        long,
        env = "CHATTY_LLAMA_URL",
        default_value = "http://192.168.0.97:11434/v1"
    )]
    llama_url: String,
}

fn default_user_data_dir(
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

fn default_database_url() -> Result<String> {
    let data_dir = default_user_data_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .context(
        "could not determine the Linux user data directory; set XDG_DATA_HOME, HOME, or CHATTY_DATABASE",
    )?;
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("create application data directory {}", data_dir.display()))?;
    Ok(format!(
        "sqlite://{}?mode=rwc",
        data_dir.join("chatty.db").display()
    ))
}

#[derive(Clone)]
struct App {
    db: SqlitePool,
    http: reqwest::Client,
    cancellations: CancellationRegistry,
    snapshot_gate: Arc<RwLock<()>>,
    deltas: broadcast::Sender<PublishedDelta>,
    recent_errors: Arc<Mutex<Vec<String>>>,
}
type Out = (MessageType, u64, Vec<u8>);
type CancellationRegistry = Arc<Mutex<HashMap<(Uuid, u64), watch::Sender<bool>>>>;
static ACTIVE_CONNECTIONS: AtomicU32 = AtomicU32::new(0);
static STARTED_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static CPU_SAMPLE: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, u64)>>> =
    std::sync::OnceLock::new();

#[derive(Clone)]
struct PublishedDelta {
    owner_id: String,
    origin: Uuid,
    delta: StateDelta,
}

fn delta_visible(identity: Option<&str>, connection_id: Uuid, event: &PublishedDelta) -> bool {
    event.origin != connection_id && identity == Some(event.owner_id.as_str())
}

struct Generation<'a> {
    tx: &'a mpsc::Sender<Out>,
    request_id: u64,
    user_id: &'a str,
    conversation_id: &'a str,
    speaker_id: Option<String>,
    parent_id: Option<String>,
    cancel: watch::Receiver<bool>,
    origin: Uuid,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install TLS crypto provider"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("chatty_broker=info".parse()?),
        )
        .init();
    let args = Args::parse();
    let _ = STARTED_AT.set(std::time::Instant::now());
    let database = match args.database.as_deref() {
        Some(database) => database.to_owned(),
        None => default_database_url()?,
    };
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database)
        .await?;
    sqlx::migrate!().run(&db).await?;
    sqlx::query("INSERT OR IGNORE INTO broker_settings(singleton,adapter_url) VALUES(1,?)")
        .bind(args.llama_url.trim_end_matches('/'))
        .execute(&db)
        .await?;
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&db)
        .await?;
    let tls = tls_config(&args.cert, &args.key)?;
    let (delta_tx, _) = broadcast::channel(256);
    let app = App {
        db,
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()?,
        cancellations: Arc::new(Mutex::new(HashMap::new())),
        snapshot_gate: Arc::new(RwLock::new(())),
        deltas: delta_tx,
        recent_errors: Arc::new(Mutex::new(Vec::new())),
    };
    let probe_app = app.clone();
    tokio::spawn(async move {
        match probe_backend(&probe_app).await {
            Ok(ids) => info!(models=?ids, "inference backend ready"),
            Err(e) => {
                warn!(error=%e, "inference backend unavailable at startup; generation will retry")
            }
        }
    });
    let listener = TcpListener::bind(&args.listen).await?;
    info!(address=%args.listen, "TLS 1.3 broker listening");
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    loop {
        let (tcp, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            match acceptor.accept(tcp).await {
                Ok(tls) => {
                    if let Err(e) = serve(tls, app).await {
                        warn!(%peer,error=%e,"connection closed")
                    }
                }
                Err(e) => warn!(%peer,error=%e,"TLS handshake rejected"),
            }
        });
    }
}

fn tls_config(cert: &str, key: &str) -> Result<ServerConfig> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(File::open(cert)?))
            .collect::<std::result::Result<_, _>>()?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(File::open(key)?))?
            .context("missing private key")?;
    Ok(
        ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    )
}

async fn serve(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    app: App,
) -> Result<()> {
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let connection_id = Uuid::new_v4();
    let identity = Arc::new(RwLock::new(None::<String>));
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::channel::<Out>(32); // bounded queue is transport backpressure
    let writer = tokio::spawn(async move {
        while let Some((ty, id, payload)) = rx.recv().await {
            if ty == MessageType::Handshake {
                // Handshake is the only client/broker JSON payload.
                write_payload(&mut wr, ty, id, payload).await?
            } else {
                // payload is already bincode: frame without re-encoding
                write_raw(&mut wr, ty, id, payload).await?;
            }
        }
        Ok::<_, ProtocolError>(())
    });
    // The sole JSON use is the version handshake.
    tx.send((
        MessageType::Handshake,
        0,
        serde_json::to_vec(
            &json!({"protocol":8,"encoding":"bincode2","compression":"zstd","tls":"1.3"}),
        )?,
    ))
    .await?;
    let mut published = app.deltas.subscribe();
    let forward_tx = tx.clone();
    let forward_identity = identity.clone();
    let (lag_tx, mut lag_rx) = watch::channel(false);
    let forwarder = tokio::spawn(async move {
        loop {
            match published.recv().await {
                Ok(event) if event.origin != connection_id => {
                    if delta_visible(
                        forward_identity.read().await.as_deref(),
                        connection_id,
                        &event,
                    ) && forward_tx
                        .send((MessageType::Delta, 0, encode(&event.delta)?))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Never leave a connected client with a silent revision gap.
                    // Closing makes the client reconnect and Resume from its last
                    // applied revision through the authoritative delta log.
                    let _ = lag_tx.send(true);
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok::<_, ProtocolError>(())
    });
    loop {
        let incoming = tokio::select! {
            frame = read_frame(&mut rd) => frame,
            changed = lag_rx.changed() => {
                if changed.is_ok() && *lag_rx.borrow() {
                    break;
                }
                continue;
            }
        };
        let frame = match incoming {
            Ok(f) => f,
            Err(ProtocolError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        match frame.message_type {
            MessageType::Request => {
                let req: Request = decode(&frame.payload)?;
                let app = app.clone();
                let error_log = app.recent_errors.clone();
                let tx = tx.clone();
                let identity = identity.clone();
                tokio::spawn(async move {
                    if let Err(e) = dispatch(
                        app,
                        tx.clone(),
                        identity,
                        connection_id,
                        frame.request_id,
                        req,
                    )
                    .await
                    {
                        let w = classify_error(&e);
                        let mut errors = error_log.lock().await;
                        errors.push(format!("{} · {e}", current_utc_timestamp()));
                        if errors.len() > 20 {
                            errors.remove(0);
                        }
                        let _ = tx
                            .send((
                                MessageType::Error,
                                frame.request_id,
                                encode(&w).unwrap_or_default(),
                            ))
                            .await;
                    }
                });
            }
            MessageType::Cancel => {
                if let Some(cancel) = app
                    .cancellations
                    .lock()
                    .await
                    .remove(&(connection_id, frame.request_id))
                {
                    let _ = cancel.send(true);
                }
            }
            _ => return Err(ProtocolError::Invalid("unexpected client message").into()),
        }
    }
    forwarder.abort();
    drop(tx);
    cancel_connection(&app.cancellations, connection_id).await;
    writer.await??;
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    Ok(())
}

async fn broker_monitor(app: &App) -> BrokerMonitor {
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

async fn adapter_health(app: &App) -> (AdapterStatus, u32, Option<u64>) {
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

async fn cancel_connection(registry: &CancellationRegistry, connection_id: Uuid) {
    let mut cancellations = registry.lock().await;
    cancellations.retain(|(id, _), cancel| {
        if *id == connection_id {
            let _ = cancel.send(true);
            false
        } else {
            true
        }
    });
}

async fn write_raw<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    ty: MessageType,
    id: u64,
    raw: Vec<u8>,
) -> Result<(), ProtocolError> {
    write_payload(w, ty, id, raw).await
}

fn classify_error(error: &anyhow::Error) -> WireError {
    let message = error.to_string();
    let lower = message.to_lowercase();
    let (code, retryable) = if lower.contains("unauthorized") || lower.contains("credentials") {
        (ErrorCode::Unauthorized, false)
    } else if lower.contains("forbidden") || lower.contains("owner") {
        (ErrorCode::Forbidden, false)
    } else if lower.contains("not found") {
        (ErrorCode::NotFound, false)
    } else if lower.contains("no model") {
        (ErrorCode::ModelMissing, true)
    } else if lower.contains("request for url")
        || lower.contains("http")
        || lower.contains("backend")
    {
        (ErrorCode::BackendUnavailable, true)
    } else if lower.contains("invalid") || lower.contains("requires") || lower.contains("must") {
        (ErrorCode::InvalidRequest, false)
    } else if lower.contains("unique constraint") {
        (ErrorCode::Conflict, false)
    } else {
        (ErrorCode::Internal, false)
    };
    WireError {
        code,
        message,
        retryable,
    }
}

async fn dispatch(
    app: App,
    tx: mpsc::Sender<Out>,
    identity: Arc<RwLock<Option<String>>>,
    connection_id: Uuid,
    id: u64,
    req: Request,
) -> Result<()> {
    let _snapshot_guard = if matches!(&req, Request::Snapshot { .. }) {
        Some(app.snapshot_gate.write().await)
    } else {
        None
    };
    let _mutation_guard = if is_mutating(&req) {
        Some(app.snapshot_gate.read().await)
    } else {
        None
    };
    macro_rules! send {
        ($ty:expr,$v:expr) => {{
            tx.send(($ty, id, encode(&$v)?)).await?;
        }};
    }
    macro_rules! send_delta {
        ($owner:expr,$revision:expr,$entity_type:expr,$entity_id:expr,$operation:expr,$changed:expr) => {{
            let delta = StateDelta {
                revision: $revision,
                entity_type: $entity_type.into(),
                entity_id: $entity_id.clone(),
                operation: $operation,
                changed_fields: $changed.clone(),
            };
            tx.send((MessageType::Delta, id, encode(&delta)?)).await?;
            let _ = app.deltas.send(PublishedDelta {
                owner_id: $owner.to_string(),
                origin: connection_id,
                delta,
            });
        }};
    }
    match req {
        Request::Register { username, password } => {
            validate_credentials(&username, &password)?;
            let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&app.db)
                .await?;
            let registration_enabled: bool = sqlx::query_scalar(
                "SELECT allow_self_registration FROM broker_settings WHERE singleton=1",
            )
            .fetch_one(&app.db)
            .await?;
            if user_count > 0 && !registration_enabled {
                bail!("self registration is disabled")
            }
            let uid = Uuid::new_v4().to_string();
            let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let hash = Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .to_string();
            sqlx::query("INSERT INTO users(id,username,password_hash,role) VALUES(?,?,?,CASE WHEN EXISTS(SELECT 1 FROM users) THEN 'user' ELSE 'admin' END)")
                .bind(&uid)
                .bind(username)
                .bind(hash)
                .execute(&app.db)
                .await?;
            let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id=?")
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
            let (token, rev) = new_session(&app.db, &uid).await?;
            *identity.write().await = Some(uid.clone());
            send!(
                MessageType::Response,
                Response::Authenticated {
                    session_token: token,
                    user_id: uid,
                    role: if role == "admin" {
                        Role::Admin
                    } else {
                        Role::User
                    },
                    revision: rev
                }
            );
        }
        Request::Login { username, password } => {
            let row = sqlx::query("SELECT id,password_hash,role FROM users WHERE username=?")
                .bind(username)
                .fetch_optional(&app.db)
                .await?
                .context("invalid credentials")?;
            let stored: String = row.get("password_hash");
            let parsed =
                PasswordHash::new(&stored).map_err(|_| anyhow::anyhow!("invalid credentials"))?;
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .map_err(|_| anyhow::anyhow!("invalid credentials"))?;
            let uid: String = row.get("id");
            let role: String = row.get("role");
            let (token, rev) = new_session(&app.db, &uid).await?;
            *identity.write().await = Some(uid.clone());
            send!(
                MessageType::Response,
                Response::Authenticated {
                    session_token: token,
                    user_id: uid,
                    role: if role == "admin" {
                        Role::Admin
                    } else {
                        Role::User
                    },
                    revision: rev
                }
            );
        }
        Request::GetServerCapabilities => {
            let registration_enabled: bool = sqlx::query_scalar(
                "SELECT allow_self_registration FROM broker_settings WHERE singleton=1",
            )
            .fetch_one(&app.db)
            .await?;
            send!(
                MessageType::Response,
                Response::ServerCapabilities {
                    registration_enabled
                }
            );
        }
        Request::Logout { session_token } => {
            sqlx::query("DELETE FROM sessions WHERE token=?")
                .bind(session_token)
                .execute(&app.db)
                .await?;
            *identity.write().await = None;
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: None,
                    revision: 0
                }
            );
        }
        Request::AdminListUsers { session_token } => {
            require_admin(&app.db, &session_token).await?;
            let rows = sqlx::query(
                "SELECT id,username,role,created_at FROM users ORDER BY created_at LIMIT 1000",
            )
            .fetch_all(&app.db)
            .await?;
            let users = rows
                .into_iter()
                .map(|r| UserAccount {
                    id: r.get("id"),
                    username: r.get("username"),
                    role: if r.get::<String, _>("role") == "admin" {
                        Role::Admin
                    } else {
                        Role::User
                    },
                    created_at: r.get("created_at"),
                })
                .collect();
            send!(MessageType::Response, Response::Users(users));
        }
        Request::AdminCreateUser {
            session_token,
            username,
            password,
            role,
        } => {
            require_admin(&app.db, &session_token).await?;
            validate_credentials(&username, &password)?;
            let uid = Uuid::new_v4().to_string();
            let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let hash = Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?
                .to_string();
            sqlx::query("INSERT INTO users(id,username,password_hash,role) VALUES(?,?,?,?)")
                .bind(&uid)
                .bind(username.trim())
                .bind(hash)
                .bind(if role == Role::Admin { "admin" } else { "user" })
                .execute(&app.db)
                .await?;
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(uid),
                    revision: 0
                }
            );
        }
        Request::AdminDeleteUser {
            session_token,
            user_id,
        } => {
            let admin_id = require_admin(&app.db, &session_token).await?;
            if admin_id == user_id {
                bail!("administrators cannot delete their own active account")
            }
            let mut transaction = app.db.begin().await?;
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=?)")
                .bind(&user_id)
                .fetch_one(&mut *transaction)
                .await?;
            if !exists {
                bail!("user not found")
            }
            sqlx::query("DELETE FROM memories WHERE owner_id=? OR character_id IN (SELECT id FROM characters WHERE owner_id=?)")
                .bind(&user_id).bind(&user_id).execute(&mut *transaction).await?;
            sqlx::query("DELETE FROM lore WHERE owner_id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM participants WHERE character_id IN (SELECT id FROM characters WHERE owner_id=?)")
                .bind(&user_id).execute(&mut *transaction).await?;
            sqlx::query("DELETE FROM conversations WHERE owner_id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM characters WHERE owner_id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM deltas WHERE owner_id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM sessions WHERE user_id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM users WHERE id=?")
                .bind(&user_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(user_id),
                    revision: 0
                }
            );
        }
        Request::GetPermissions { session_token } => {
            let role:String=sqlx::query_scalar("SELECT u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')").bind(session_token).fetch_optional(&app.db).await?.context("unauthorized")?;
            let mut permissions = vec![Permission::ManageOwnRoleplay, Permission::GenerateRoleplay];
            if role == "admin" {
                permissions.push(Permission::ManageUsers);
            }
            send!(MessageType::Response, Response::Permissions(permissions));
        }
        Request::AdminSetRole {
            session_token,
            user_id,
            role,
        } => {
            let admin_id = require_admin(&app.db, &session_token).await?;
            if admin_id == user_id && role == Role::User {
                bail!("administrators cannot demote their own active account")
            }
            let changed = sqlx::query("UPDATE users SET role=? WHERE id=?")
                .bind(if role == Role::Admin { "admin" } else { "user" })
                .bind(&user_id)
                .execute(&app.db)
                .await?
                .rows_affected();
            if changed != 1 {
                bail!("user not found")
            }
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(user_id),
                    revision: 0
                }
            );
        }
        Request::AdminGetBrokerConfig { session_token } => {
            require_admin(&app.db, &session_token).await?;
            send!(
                MessageType::Response,
                Response::BrokerConfig(load_broker_config(&app.db).await?)
            );
        }
        Request::AdminGetBrokerMonitor { session_token } => {
            require_admin(&app.db, &session_token).await?;
            send!(
                MessageType::Response,
                Response::BrokerMonitor(broker_monitor(&app).await)
            );
        }
        Request::AdminSoftReboot { session_token } => {
            require_admin(&app.db, &session_token).await?;
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: None,
                    revision: 0
                }
            );
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                std::process::exit(0);
            });
        }
        Request::AdminSetBrokerConfig {
            session_token,
            mut config,
        } => {
            require_admin(&app.db, &session_token).await?;
            config.adapter_url = config.adapter_url.trim().trim_end_matches('/').to_owned();
            if !config.adapter_url.starts_with("http://")
                && !config.adapter_url.starts_with("https://")
            {
                bail!("adapter URL must use http or https")
            }
            sqlx::query("UPDATE broker_settings SET adapter_enabled=?,adapter_url=?,allow_public_characters=?,allow_self_registration=?,updated_at=CURRENT_TIMESTAMP WHERE singleton=1")
                .bind(config.adapter_enabled)
                .bind(&config.adapter_url)
                .bind(config.allow_public_characters)
                .bind(config.allow_self_registration)
                .execute(&app.db)
                .await?;
            send!(MessageType::Response, Response::BrokerConfig(config));
        }
        Request::AdminSetCharacterPublic {
            session_token,
            character_id,
            is_public,
        } => {
            require_admin(&app.db, &session_token).await?;
            let changed = sqlx::query("UPDATE characters SET is_public=? WHERE id=?")
                .bind(is_public)
                .bind(&character_id)
                .execute(&app.db)
                .await?
                .rows_affected();
            if changed != 1 {
                bail!("character not found")
            }
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(character_id),
                    revision: 0
                }
            );
        }
        Request::AdminReadDatabase { session_token } => {
            require_admin(&app.db, &session_token).await?;
            let mut data = Vec::new();
            for row in sqlx::query(
                "SELECT id,username,role,created_at FROM users ORDER BY created_at LIMIT 1000",
            )
            .fetch_all(&app.db)
            .await?
            {
                data.push(AdminDataRow {
                    kind: "User".into(),
                    id: row.get("id"),
                    label: row.get("username"),
                    detail: format!(
                        "{} · {}",
                        row.get::<String, _>("role"),
                        row.get::<String, _>("created_at")
                    ),
                    is_public: None,
                });
            }
            for row in sqlx::query("SELECT c.id,c.name,c.is_public,u.username FROM characters c JOIN users u ON u.id=c.owner_id ORDER BY c.name LIMIT 1000").fetch_all(&app.db).await? {
                data.push(AdminDataRow { kind: "Character".into(), id: row.get("id"), label: row.get("name"), detail: format!("Owner: {}", row.get::<String, _>("username")), is_public: Some(row.get("is_public")) });
            }
            let config = load_broker_config(&app.db).await?;
            data.push(AdminDataRow {
                kind: "Setting".into(),
                id: "adapter".into(),
                label: "Adapter".into(),
                detail: format!(
                    "{} · {}",
                    if config.adapter_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    config.adapter_url
                ),
                is_public: None,
            });
            send!(MessageType::Response, Response::AdminDatabase(data));
        }
        Request::ListCharacters { session_token } => {
            let uid = auth(&app.db, &session_token).await?;
            let rows = sqlx::query(
                "SELECT * FROM characters WHERE owner_id=? OR is_public=1 ORDER BY name LIMIT 500",
            )
            .bind(&uid)
            .fetch_all(&app.db)
            .await?;
            let cs = rows
                .into_iter()
                .map(|r| Character {
                    id: r.get("id"),
                    name: r.get("name"),
                    personality: r.get("personality"),
                    scenario: r.get("scenario"),
                    system_prompt: r.get("system_prompt"),
                    example_dialogue: r.get("example_dialogue"),
                    appearance: r.get("appearance"),
                    tags: decode(r.get::<&[u8], _>("tags")).unwrap_or_default(),
                    avatar: r.get("avatar"),
                    is_public: r.get("is_public"),
                    owned_by_user: r.get::<String, _>("owner_id") == uid,
                    revision: r.get("revision"),
                })
                .collect();
            send!(MessageType::Response, Response::Characters(cs));
        }
        Request::ListConversations { session_token } => {
            let uid = auth(&app.db, &session_token).await?;
            let rows = sqlx::query("SELECT id,title,kind,CAST(state AS TEXT) AS state,summary,revision FROM conversations WHERE owner_id=? ORDER BY revision DESC LIMIT 500")
                .bind(&uid).fetch_all(&app.db).await?;
            let mut conversations = Vec::with_capacity(rows.len());
            for row in rows {
                conversations.push(conversation_from_row(&app.db, row).await?);
            }
            send!(
                MessageType::Response,
                Response::Conversations(conversations)
            );
        }
        Request::GetConversation {
            session_token,
            conversation_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            own_conversation(&app.db, &uid, &conversation_id).await?;
            send!(
                MessageType::Response,
                Response::ConversationView(load_conversation(&app.db, &conversation_id).await?)
            );
        }
        Request::UpdateConversationState {
            session_token,
            conversation_id,
            state,
            summary,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if state.len() > 262_144 || summary.len() > 262_144 {
                bail!("conversation state or summary too large")
            }
            own_conversation(&app.db, &uid, &conversation_id).await?;
            let changed = encode(&DeltaPayload::ConversationContext {
                state: state.clone(),
                summary: summary.clone(),
            })?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "conversation",
                &conversation_id,
                DeltaOperation::Update,
                &changed,
            )
            .await?;
            sqlx::query("UPDATE conversations SET state=?,summary=?,revision=? WHERE id=?")
                .bind(state)
                .bind(summary)
                .bind(rev)
                .bind(&conversation_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            send_delta!(
                &uid,
                rev,
                "conversation",
                conversation_id,
                DeltaOperation::Update,
                changed
            );
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(conversation_id),
                    revision: rev
                }
            );
        }
        Request::DeleteEntity {
            session_token,
            kind,
            entity_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            let mut transaction = app.db.begin().await?;
            let deletions = delete_owned_entity(&mut transaction, &uid, kind, &entity_id).await?;
            let empty = encode(&DeltaPayload::Empty)?;
            let mut rev = 0;
            let mut outgoing = Vec::new();
            for (entity_type, id) in deletions {
                rev = delta_tx(
                    &mut transaction,
                    &uid,
                    entity_type,
                    &id,
                    DeltaOperation::Delete,
                    &empty,
                )
                .await?;
                outgoing.push(StateDelta {
                    revision: rev,
                    entity_type: entity_type.into(),
                    entity_id: id,
                    operation: DeltaOperation::Delete,
                    changed_fields: empty.clone(),
                });
            }
            transaction.commit().await?;
            for delta in outgoing {
                tx.send((MessageType::Delta, id, encode(&delta)?)).await?;
                let _ = app.deltas.send(PublishedDelta {
                    owner_id: uid.clone(),
                    origin: connection_id,
                    delta,
                });
            }
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(entity_id),
                    revision: rev
                }
            );
        }
        Request::ListLore {
            session_token,
            conversation_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if let Some(cid) = &conversation_id {
                own_conversation(&app.db, &uid, cid).await?;
            }
            let rows=sqlx::query("SELECT id,conversation_id,keywords,content,always_on,priority,revision FROM lore WHERE owner_id=? AND (? IS NULL OR conversation_id=?) ORDER BY priority DESC LIMIT 500")
                .bind(&uid).bind(&conversation_id).bind(&conversation_id).fetch_all(&app.db).await?;
            let entries = rows
                .into_iter()
                .map(|r| LoreEntry {
                    id: r.get("id"),
                    conversation_id: r.get("conversation_id"),
                    keywords: decode(r.get::<&[u8], _>("keywords")).unwrap_or_default(),
                    content: r.get("content"),
                    always_on: r.get("always_on"),
                    priority: r.get("priority"),
                    revision: r.get("revision"),
                })
                .collect();
            send!(MessageType::Response, Response::Lore(entries));
        }
        Request::ListMemories {
            session_token,
            conversation_id,
            character_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if let Some(cid) = &conversation_id {
                own_conversation(&app.db, &uid, cid).await?;
            }
            let rows=sqlx::query("SELECT id,conversation_id,character_id,content,revision FROM memories WHERE owner_id=? AND (? IS NULL OR conversation_id=?) AND (? IS NULL OR character_id=?) ORDER BY revision DESC LIMIT 500")
                .bind(&uid).bind(&conversation_id).bind(&conversation_id).bind(&character_id).bind(&character_id).fetch_all(&app.db).await?;
            let entries = rows
                .into_iter()
                .map(|r| MemoryEntry {
                    id: r.get("id"),
                    conversation_id: r.get("conversation_id"),
                    character_id: r.get("character_id"),
                    content: r.get("content"),
                    revision: r.get("revision"),
                })
                .collect();
            send!(MessageType::Response, Response::Memories(entries));
        }
        Request::UpsertCharacter {
            session_token,
            character,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            validate_character(&character)?;
            if character.is_public {
                let config = load_broker_config(&app.db).await?;
                let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id=?")
                    .bind(&uid)
                    .fetch_one(&app.db)
                    .await?;
                if role != "admin" && !config.allow_public_characters {
                    bail!("publishing characters is disabled")
                }
            }
            let mut operation = DeltaOperation::Add;
            if let Some(id) = &character.id {
                let owned: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND owner_id=?)",
                )
                .bind(id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM characters WHERE id=?)")
                        .bind(id)
                        .fetch_one(&app.db)
                        .await?;
                if exists && !owned {
                    bail!("forbidden character owner")
                }
                if owned {
                    operation = DeltaOperation::Update;
                }
            }
            let mut visible_character = character.clone();
            visible_character.owned_by_user = true;
            let changed = encode(&DeltaPayload::Character(visible_character))?;
            let eid = character.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let fields = encode(&character.tags)?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "character",
                &eid,
                operation,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO characters(id,owner_id,name,personality,scenario,system_prompt,example_dialogue,appearance,tags,avatar,revision,is_public) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,personality=excluded.personality,scenario=excluded.scenario,system_prompt=excluded.system_prompt,example_dialogue=excluded.example_dialogue,appearance=excluded.appearance,tags=excluded.tags,avatar=excluded.avatar,revision=excluded.revision,is_public=excluded.is_public WHERE owner_id=excluded.owner_id").bind(&eid).bind(&uid).bind(character.name).bind(character.personality).bind(character.scenario).bind(character.system_prompt).bind(character.example_dialogue).bind(character.appearance).bind(fields).bind(character.avatar).bind(rev).bind(character.is_public).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "character", eid, operation, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::CreateConversation {
            session_token,
            title,
            kind,
            mut participant_ids,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if participant_ids.is_empty() {
                if !matches!(kind, ConversationKind::Direct) {
                    bail!("group conversation requires participants")
                }
                let existing: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM characters WHERE owner_id=? ORDER BY revision,id LIMIT 1",
                )
                .bind(&uid)
                .fetch_optional(&app.db)
                .await?;
                let participant_id = if let Some(id) = existing {
                    id
                } else {
                    let id = Uuid::new_v4().to_string();
                    let character = CharacterInput {
                        id: Some(id.clone()),
                        name: "Assistant".into(),
                        personality: "Helpful, attentive, and conversational.".into(),
                        scenario: String::new(),
                        system_prompt:
                            "Respond naturally and remain consistent with the conversation.".into(),
                        example_dialogue: String::new(),
                        appearance: String::new(),
                        tags: vec!["default".into()],
                        avatar: None,
                        is_public: false,
                        owned_by_user: true,
                    };
                    let changed = encode(&DeltaPayload::Character(character.clone()))?;
                    let tags = encode(&character.tags)?;
                    let mut transaction = app.db.begin().await?;
                    let rev = delta_tx(
                        &mut transaction,
                        &uid,
                        "character",
                        &id,
                        DeltaOperation::Add,
                        &changed,
                    )
                    .await?;
                    sqlx::query("INSERT INTO characters(id,owner_id,name,personality,scenario,system_prompt,example_dialogue,appearance,tags,avatar,revision,is_public) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
                        .bind(&id)
                        .bind(&uid)
                        .bind(character.name)
                        .bind(character.personality)
                        .bind(character.scenario)
                        .bind(character.system_prompt)
                        .bind(character.example_dialogue)
                        .bind(character.appearance)
                        .bind(tags)
                        .bind(character.avatar)
                        .bind(rev)
                        .bind(false)
                        .execute(&mut *transaction)
                        .await?;
                    transaction.commit().await?;
                    send_delta!(&uid, rev, "character", id, DeltaOperation::Add, changed);
                    id
                };
                participant_ids.push(participant_id);
            }
            if participant_ids.len() > 32 || title.is_empty() || title.len() > 512 {
                bail!("conversation title or participant count invalid")
            }
            let participant_json = serde_json::to_string(&participant_ids)?;
            let owned_count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM characters WHERE (owner_id=? OR is_public=1) AND id IN (SELECT value FROM json_each(?))").bind(&uid).bind(participant_json).fetch_one(&app.db).await?;
            if owned_count != participant_ids.len() as i64 {
                bail!("one or more participants are missing or forbidden")
            }
            let eid = Uuid::new_v4().to_string();
            let changed = encode(&DeltaPayload::Conversation {
                title: title.clone(),
                kind,
                participant_ids: participant_ids.clone(),
                state: String::new(),
                summary: String::new(),
            })?;
            let mut t = app.db.begin().await?;
            let rev = delta_tx(
                &mut t,
                &uid,
                "conversation",
                &eid,
                DeltaOperation::Add,
                &changed,
            )
            .await?;
            sqlx::query(
                "INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES(?,?,?,?,?)",
            )
            .bind(&eid)
            .bind(&uid)
            .bind(title)
            .bind(kind as i32)
            .bind(rev)
            .execute(&mut *t)
            .await?;
            for (n, c) in participant_ids.iter().enumerate() {
                sqlx::query("INSERT INTO participants(conversation_id,character_id,position) SELECT ?,id,? FROM characters WHERE id=? AND (owner_id=? OR is_public=1)").bind(&eid).bind(n as i32).bind(c).bind(&uid).execute(&mut *t).await?;
            }
            t.commit().await?;
            send_delta!(&uid, rev, "conversation", eid, DeltaOperation::Add, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::UpdateConversation {
            session_token,
            conversation_id,
            title,
            participant_ids,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            own_conversation(&app.db, &uid, &conversation_id).await?;
            if participant_ids.is_empty()
                || participant_ids.len() > 32
                || title.is_empty()
                || title.len() > 512
            {
                bail!("conversation title or participant count invalid")
            }
            let participant_json = serde_json::to_string(&participant_ids)?;
            let owned_count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM characters WHERE (owner_id=? OR is_public=1) AND id IN(SELECT value FROM json_each(?))").bind(&uid).bind(participant_json).fetch_one(&app.db).await?;
            if owned_count != participant_ids.len() as i64 {
                bail!("one or more participants are missing or forbidden")
            }
            let current = sqlx::query(
                "SELECT kind,CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=?",
            )
            .bind(&conversation_id)
            .fetch_one(&app.db)
            .await?;
            let kind = ConversationKind::try_from(current.get::<i32, _>("kind"))?;
            let state: String = current.get("state");
            let summary: String = current.get("summary");
            let changed = encode(&DeltaPayload::Conversation {
                title: title.clone(),
                kind,
                participant_ids: participant_ids.clone(),
                state,
                summary,
            })?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "conversation",
                &conversation_id,
                DeltaOperation::Update,
                &changed,
            )
            .await?;
            sqlx::query("UPDATE conversations SET title=?,revision=? WHERE id=?")
                .bind(title)
                .bind(rev)
                .bind(&conversation_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM participants WHERE conversation_id=?")
                .bind(&conversation_id)
                .execute(&mut *transaction)
                .await?;
            for (position, character_id) in participant_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO participants(conversation_id,character_id,position) VALUES(?,?,?)",
                )
                .bind(&conversation_id)
                .bind(character_id)
                .bind(position as i32)
                .execute(&mut *transaction)
                .await?;
            }
            transaction.commit().await?;
            send_delta!(
                &uid,
                rev,
                "conversation",
                conversation_id,
                DeltaOperation::Update,
                changed
            );
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(conversation_id),
                    revision: rev
                }
            );
        }
        Request::SendMessage {
            session_token,
            conversation_id,
            content,
            speaker_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if content.is_empty() || content.len() > 65_536 {
                bail!("message length invalid")
            }
            own_conversation(&app.db, &uid, &conversation_id).await?;
            if let Some(speaker) = &speaker_id {
                let participant:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM participants WHERE conversation_id=? AND character_id=?)").bind(&conversation_id).bind(speaker).fetch_one(&app.db).await?;
                if !participant {
                    bail!("message speaker is not a conversation participant")
                }
            }
            let eid = Uuid::new_v4().to_string();
            let author_type = if speaker_id.is_some() {
                "character"
            } else {
                "user"
            };
            let changed = encode(&DeltaPayload::Message {
                conversation_id: conversation_id.clone(),
                author_type: author_type.into(),
                author_id: speaker_id.clone(),
                content: content.clone(),
                parent_id: None,
                selected_variant_id: None,
            })?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "message",
                &eid,
                DeltaOperation::Add,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES(?,?,?,?,?,?)").bind(&eid).bind(&conversation_id).bind(author_type).bind(&speaker_id).bind(&content).bind(rev).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "message", eid, DeltaOperation::Add, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
            if speaker_id.is_none()
                && let Some(delta) =
                    maybe_name_new_chat(&app, &uid, &conversation_id, &content).await?
            {
                tx.send((MessageType::Delta, id, encode(&delta)?)).await?;
                let _ = app.deltas.send(PublishedDelta {
                    owner_id: uid,
                    origin: connection_id,
                    delta,
                });
            }
        }
        Request::SendSystemMessage {
            session_token,
            conversation_id,
            content,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if content.is_empty() || content.len() > 65_536 {
                bail!("message length invalid")
            }
            own_conversation(&app.db, &uid, &conversation_id).await?;
            let eid = Uuid::new_v4().to_string();
            let changed = encode(&DeltaPayload::Message {
                conversation_id: conversation_id.clone(),
                author_type: "system".into(),
                author_id: None,
                content: content.clone(),
                parent_id: None,
                selected_variant_id: None,
            })?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "message",
                &eid,
                DeltaOperation::Add,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES(?,?,'system',NULL,?,?)").bind(&eid).bind(conversation_id).bind(content).bind(rev).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "message", eid, DeltaOperation::Add, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::Generate {
            session_token,
            conversation_id,
            speaker_id,
            parent_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            own_conversation(&app.db, &uid, &conversation_id).await?;
            if let Some(parent) = &parent_id {
                let valid: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE id=? AND conversation_id=?)",
                )
                .bind(parent)
                .bind(&conversation_id)
                .fetch_one(&app.db)
                .await?;
                if !valid {
                    bail!("variant parent not found in conversation")
                }
            }
            let (cancel, cancel_rx) = watch::channel(false);
            app.cancellations
                .lock()
                .await
                .insert((connection_id, id), cancel);
            let result = generate(
                &app,
                Generation {
                    tx: &tx,
                    request_id: id,
                    user_id: &uid,
                    conversation_id: &conversation_id,
                    speaker_id,
                    parent_id,
                    cancel: cancel_rx,
                    origin: connection_id,
                },
            )
            .await;
            app.cancellations.lock().await.remove(&(connection_id, id));
            result?;
        }
        Request::Snapshot { session_token } => {
            let uid = auth(&app.db, &session_token).await?;
            *identity.write().await = Some(uid.clone());
            send_snapshot(&app, &tx, id, &uid).await?;
        }
        request @ (Request::Sync { .. } | Request::Resume { .. }) => {
            let is_resume = matches!(&request, Request::Resume { .. });
            let (session_token, since_revision) = match request {
                Request::Sync {
                    session_token,
                    since_revision,
                }
                | Request::Resume {
                    session_token,
                    since_revision,
                } => (session_token, since_revision),
                _ => unreachable!(),
            };
            let uid = auth(&app.db, &session_token).await?;
            *identity.write().await = Some(uid.clone());
            if is_resume {
                let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id=?")
                    .bind(&uid)
                    .fetch_one(&app.db)
                    .await?;
                send!(
                    MessageType::Response,
                    Response::Authenticated {
                        session_token: session_token.clone(),
                        user_id: uid.clone(),
                        role: if role == "admin" {
                            Role::Admin
                        } else {
                            Role::User
                        },
                        revision: since_revision
                    }
                );
            }
            let mut rows=sqlx::query("SELECT revision,entity_type,entity_id,operation,changed_fields FROM deltas WHERE owner_id=? AND revision>? ORDER BY revision").bind(uid).bind(since_revision).fetch(&app.db);
            let mut last = since_revision;
            while let Some(r) = rows.try_next().await? {
                last = r.get("revision");
                let d = StateDelta {
                    revision: last,
                    entity_type: r.get("entity_type"),
                    entity_id: r.get("entity_id"),
                    operation: match r.get::<i32, _>("operation") {
                        0 => DeltaOperation::Add,
                        1 => DeltaOperation::Update,
                        _ => DeltaOperation::Delete,
                    },
                    changed_fields: r.get("changed_fields"),
                };
                send!(MessageType::Delta, d);
            }
            send!(
                MessageType::Response,
                Response::SyncComplete { revision: last }
            );
        }
        Request::UpsertLore {
            session_token,
            lore,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if lore.content.is_empty()
                || lore.content.len() > 65_536
                || lore.keywords.len() > 128
                || lore.keywords.iter().any(|k| k.len() > 256)
            {
                bail!("lore size invalid")
            }
            if let Some(cid) = &lore.conversation_id {
                own_conversation(&app.db, &uid, cid).await?;
            }
            let mut operation = DeltaOperation::Add;
            if let Some(id) = &lore.id {
                let allowed: bool = sqlx::query_scalar(
                    "SELECT NOT EXISTS(SELECT 1 FROM lore WHERE id=? AND owner_id<>?)",
                )
                .bind(id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                if !allowed {
                    bail!("forbidden lore owner")
                }
                let owned: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM lore WHERE id=? AND owner_id=?)",
                )
                .bind(id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                if owned {
                    operation = DeltaOperation::Update;
                }
            }
            let changed = encode(&DeltaPayload::Lore(lore.clone()))?;
            let eid = lore.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let kw = encode(&lore.keywords)?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(&mut transaction, &uid, "lore", &eid, operation, &changed).await?;
            sqlx::query("INSERT INTO lore VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET keywords=excluded.keywords,content=excluded.content,always_on=excluded.always_on,priority=excluded.priority,revision=excluded.revision WHERE owner_id=excluded.owner_id").bind(&eid).bind(&uid).bind(lore.conversation_id).bind(kw).bind(lore.content).bind(lore.always_on).bind(lore.priority).bind(rev).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "lore", eid, operation, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::UpsertMemory {
            session_token,
            memory,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if memory.content.is_empty() || memory.content.len() > 65_536 {
                bail!("memory size invalid")
            }
            if let Some(cid) = &memory.conversation_id {
                own_conversation(&app.db, &uid, cid).await?;
            }
            if let Some(character_id) = &memory.character_id {
                let owned: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND (owner_id=? OR is_public=1))",
                )
                .bind(character_id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                if !owned {
                    bail!("memory character missing or forbidden")
                }
            }
            let mut operation = DeltaOperation::Add;
            if let Some(id) = &memory.id {
                let allowed: bool = sqlx::query_scalar(
                    "SELECT NOT EXISTS(SELECT 1 FROM memories WHERE id=? AND owner_id<>?)",
                )
                .bind(id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                if !allowed {
                    bail!("forbidden memory owner")
                }
                let owned: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM memories WHERE id=? AND owner_id=?)",
                )
                .bind(id)
                .bind(&uid)
                .fetch_one(&app.db)
                .await?;
                if owned {
                    operation = DeltaOperation::Update;
                }
            }
            let changed = encode(&DeltaPayload::Memory(memory.clone()))?;
            let eid = memory.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(&mut transaction, &uid, "memory", &eid, operation, &changed).await?;
            sqlx::query("INSERT INTO memories VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET content=excluded.content,revision=excluded.revision WHERE owner_id=excluded.owner_id").bind(&eid).bind(&uid).bind(memory.conversation_id).bind(memory.character_id).bind(memory.content).bind(rev).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "memory", eid, operation, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::ExtractMemory {
            session_token,
            conversation_id,
            character_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            own_conversation(&app.db, &uid, &conversation_id).await?;
            if let Some(character_id) = &character_id {
                let participant: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? AND c.id=? AND c.owner_id=?)")
                    .bind(&conversation_id).bind(character_id).bind(&uid).fetch_one(&app.db).await?;
                if !participant {
                    bail!("memory character is not a conversation participant")
                }
            }
            let content = extract_memory(&app, &conversation_id).await?;
            let eid = Uuid::new_v4().to_string();
            let memory = MemoryInput {
                id: Some(eid.clone()),
                conversation_id: Some(conversation_id),
                character_id,
                content,
            };
            let changed = encode(&DeltaPayload::Memory(memory.clone()))?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "memory",
                &eid,
                DeltaOperation::Add,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO memories(id,owner_id,conversation_id,character_id,content,revision) VALUES(?,?,?,?,?,?)")
                .bind(&eid).bind(&uid).bind(&memory.conversation_id).bind(&memory.character_id).bind(&memory.content).bind(rev)
                .execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "memory", eid, DeltaOperation::Add, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision: rev
                }
            );
        }
        Request::SelectVariant {
            session_token,
            message_id,
            variant_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE m.id=? AND c.owner_id=? AND (?=m.id OR EXISTS(SELECT 1 FROM variants v WHERE v.message_id=m.id AND v.id=?)))").bind(&message_id).bind(&uid).bind(&variant_id).bind(&variant_id).fetch_one(&app.db).await?;
            if !valid {
                bail!("message or variant not found")
            }
            let selected = (variant_id != message_id).then_some(variant_id);
            let changed = encode(&DeltaPayload::VariantSelection {
                variant_id: selected.clone(),
            })?;
            let mut transaction = app.db.begin().await?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "message",
                &message_id,
                DeltaOperation::Update,
                &changed,
            )
            .await?;
            sqlx::query("UPDATE messages SET selected_variant_id=?,revision=? WHERE id=?")
                .bind(&selected)
                .bind(rev)
                .bind(&message_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            send_delta!(
                &uid,
                rev,
                "message",
                message_id,
                DeltaOperation::Update,
                changed
            );
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(message_id),
                    revision: rev
                }
            );
        }
    }
    Ok(())
}

fn is_mutating(request: &Request) -> bool {
    matches!(
        request,
        Request::Register { .. }
            | Request::Logout { .. }
            | Request::AdminSetRole { .. }
            | Request::AdminCreateUser { .. }
            | Request::AdminDeleteUser { .. }
            | Request::AdminSetBrokerConfig { .. }
            | Request::AdminSetCharacterPublic { .. }
            | Request::UpsertCharacter { .. }
            | Request::CreateConversation { .. }
            | Request::UpdateConversation { .. }
            | Request::UpdateConversationState { .. }
            | Request::DeleteEntity { .. }
            | Request::SendMessage { .. }
            | Request::SendSystemMessage { .. }
            | Request::Generate { .. }
            | Request::SelectVariant { .. }
            | Request::UpsertLore { .. }
            | Request::UpsertMemory { .. }
            | Request::ExtractMemory { .. }
    )
}

fn validate_credentials(u: &str, p: &str) -> Result<()> {
    if u.len() < 3 || u.len() > 64 || p.len() < 10 || p.len() > 1024 {
        bail!("username or password length invalid")
    }
    Ok(())
}
fn validate_character(character: &CharacterInput) -> Result<()> {
    if character.name.is_empty() || character.name.len() > 256 {
        bail!("character name length invalid")
    }
    for value in [
        &character.personality,
        &character.scenario,
        &character.system_prompt,
        &character.example_dialogue,
        &character.appearance,
    ] {
        if value.len() > 65_536 {
            bail!("character field too large")
        }
    }
    if character.tags.len() > 128 || character.tags.iter().any(|tag| tag.len() > 256) {
        bail!("character tags too large")
    }
    if character
        .avatar
        .as_ref()
        .is_some_and(|avatar| avatar.len() > 2 * 1024 * 1024)
    {
        bail!("character avatar too large")
    }
    Ok(())
}
async fn new_session(db: &SqlitePool, uid: &str) -> Result<(String, i64)> {
    let token = Uuid::new_v4().to_string() + &Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO sessions(token,user_id) VALUES(?,?)")
        .bind(&token)
        .bind(uid)
        .execute(db)
        .await?;
    let rev = sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas")
        .fetch_one(db)
        .await?;
    Ok((token, rev))
}
async fn auth(db: &SqlitePool, t: &str) -> Result<String> {
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=? AND expires_at>datetime('now')")
        .bind(t)
        .fetch_optional(db)
        .await?
        .context("unauthorized")
}
async fn require_admin(db: &SqlitePool, token: &str) -> Result<String> {
    let row=sqlx::query("SELECT u.id,u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')").bind(token).fetch_optional(db).await?.context("unauthorized")?;
    if row.get::<String, _>("role") != "admin" {
        bail!("forbidden")
    }
    Ok(row.get("id"))
}

async fn load_broker_config(db: &SqlitePool) -> Result<BrokerConfig> {
    let row = sqlx::query("SELECT adapter_enabled,adapter_url,allow_public_characters,allow_self_registration FROM broker_settings WHERE singleton=1")
        .fetch_one(db)
        .await?;
    Ok(BrokerConfig {
        adapter_enabled: row.get("adapter_enabled"),
        adapter_url: row.get("adapter_url"),
        allow_public_characters: row.get("allow_public_characters"),
        allow_self_registration: row.get("allow_self_registration"),
    })
}

async fn adapter_url(app: &App) -> Result<String> {
    let config = load_broker_config(&app.db).await?;
    if !config.adapter_enabled {
        bail!("inference adapter is disabled")
    }
    Ok(config.adapter_url)
}
async fn own_conversation(db: &SqlitePool, u: &str, c: &str) -> Result<()> {
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=?)",
    )
    .bind(c)
    .bind(u)
    .fetch_one(db)
    .await?
    {
        bail!("conversation not found")
    }
    Ok(())
}

async fn delete_owned_entity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    kind: EntityKind,
    entity_id: &str,
) -> Result<Vec<(&'static str, String)>> {
    let mut deleted = Vec::new();
    match kind {
        EntityKind::Character => {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND owner_id=?)",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let in_use: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM participants WHERE character_id=?)",
            )
            .bind(entity_id)
            .fetch_one(&mut **transaction)
            .await?;
            if in_use {
                bail!("character is used by a conversation; delete that conversation first")
            }
            let memories: Vec<String> =
                sqlx::query_scalar("SELECT id FROM memories WHERE character_id=? AND owner_id=?")
                    .bind(entity_id)
                    .bind(user_id)
                    .fetch_all(&mut **transaction)
                    .await?;
            sqlx::query("DELETE FROM memories WHERE character_id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.extend(memories.into_iter().map(|id| ("memory", id)));
            sqlx::query("DELETE FROM characters WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.push(("character", entity_id.into()));
        }
        EntityKind::Conversation => {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=?)",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let messages: Vec<String> =
                sqlx::query_scalar("SELECT id FROM messages WHERE conversation_id=?")
                    .bind(entity_id)
                    .fetch_all(&mut **transaction)
                    .await?;
            let lore: Vec<String> =
                sqlx::query_scalar("SELECT id FROM lore WHERE conversation_id=? AND owner_id=?")
                    .bind(entity_id)
                    .bind(user_id)
                    .fetch_all(&mut **transaction)
                    .await?;
            let memories: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM memories WHERE conversation_id=? AND owner_id=?",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_all(&mut **transaction)
            .await?;
            sqlx::query("DELETE FROM lore WHERE conversation_id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("DELETE FROM memories WHERE conversation_id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("DELETE FROM conversations WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.extend(messages.into_iter().map(|id| ("message", id)));
            deleted.extend(lore.into_iter().map(|id| ("lore", id)));
            deleted.extend(memories.into_iter().map(|id| ("memory", id)));
            deleted.push(("conversation", entity_id.into()));
        }
        EntityKind::Message => {
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE m.id=? AND c.owner_id=?)").bind(entity_id).bind(user_id).fetch_one(&mut **transaction).await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let ids:Vec<String>=sqlx::query_scalar("WITH RECURSIVE branch(id) AS (SELECT ? UNION ALL SELECT m.id FROM messages m JOIN branch b ON m.parent_id=b.id) SELECT id FROM branch").bind(entity_id).fetch_all(&mut **transaction).await?;
            sqlx::query("WITH RECURSIVE branch(id) AS (SELECT ? UNION ALL SELECT m.id FROM messages m JOIN branch b ON m.parent_id=b.id) DELETE FROM messages WHERE id IN(SELECT id FROM branch)").bind(entity_id).execute(&mut **transaction).await?;
            deleted.extend(ids.into_iter().map(|id| ("message", id)));
        }
        EntityKind::Lore => {
            let affected = sqlx::query("DELETE FROM lore WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?
                .rows_affected();
            if affected != 1 {
                bail!("entity not found or forbidden")
            };
            deleted.push(("lore", entity_id.into()));
        }
        EntityKind::Memory => {
            let affected = sqlx::query("DELETE FROM memories WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?
                .rows_affected();
            if affected != 1 {
                bail!("entity not found or forbidden")
            };
            deleted.push(("memory", entity_id.into()));
        }
    }
    Ok(deleted)
}
async fn delta_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    entity_type: &str,
    entity_id: &str,
    operation: DeltaOperation,
    changed_fields: &[u8],
) -> Result<i64> {
    let result = sqlx::query("INSERT INTO deltas(owner_id,entity_type,entity_id,operation,changed_fields) VALUES(?,?,?,?,?)")
        .bind(user_id).bind(entity_type).bind(entity_id)
        .bind(match operation { DeltaOperation::Add => 0, DeltaOperation::Update => 1, DeltaOperation::Delete => 2 })
        .bind(changed_fields).execute(&mut **transaction).await?;
    Ok(result.last_insert_rowid())
}

async fn conversation_from_row(
    db: &SqlitePool,
    row: sqlx::sqlite::SqliteRow,
) -> Result<Conversation> {
    let id: String = row.get("id");
    let participant_ids = sqlx::query_scalar(
        "SELECT character_id FROM participants WHERE conversation_id=? ORDER BY position",
    )
    .bind(&id)
    .fetch_all(db)
    .await?;
    Ok(Conversation {
        id,
        title: row.get("title"),
        kind: ConversationKind::try_from(row.get::<i32, _>("kind"))?,
        participant_ids,
        state: row.get("state"),
        summary: row.get("summary"),
        revision: row.get("revision"),
    })
}

async fn load_conversation(db: &SqlitePool, id: &str) -> Result<ConversationView> {
    let row = sqlx::query("SELECT id,title,kind,CAST(state AS TEXT) AS state,summary,revision FROM conversations WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await?;
    let conversation = conversation_from_row(db, row).await?;
    let rows = sqlx::query("SELECT id,author_type,author_id,content,parent_id,selected_variant_id,created_at,revision FROM messages WHERE conversation_id=? AND parent_id IS NULL ORDER BY created_at,id LIMIT 2000")
        .bind(id).fetch_all(db).await?;
    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        let message_id: String = row.get("id");
        let variant_rows = sqlx::query(
            "SELECT id,content,created_at,revision FROM variants WHERE message_id=? ORDER BY created_at,id",
        )
        .bind(&message_id)
        .fetch_all(db)
        .await?;
        let variants = variant_rows
            .into_iter()
            .map(|v| Variant {
                id: v.get("id"),
                content: v.get("content"),
                created_at: v.get("created_at"),
                revision: v.get("revision"),
            })
            .collect();
        messages.push(ChatMessage {
            id: message_id,
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
            created_at: row.get("created_at"),
            revision: row.get("revision"),
            variants,
        });
    }
    Ok(ConversationView {
        conversation,
        messages,
    })
}

async fn send_snapshot(
    app: &App,
    tx: &mpsc::Sender<Out>,
    request_id: u64,
    user_id: &str,
) -> Result<()> {
    let snapshot_revision: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas WHERE owner_id=?")
            .bind(user_id)
            .fetch_one(&app.db)
            .await?;
    let mut characters = sqlx::query(
        "SELECT * FROM characters WHERE (owner_id=? AND revision<=?) OR is_public=1 ORDER BY id",
    )
    .bind(user_id)
    .bind(snapshot_revision)
    .fetch(&app.db);
    while let Some(row) = characters.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Character(CharacterInput {
            id: Some(entity_id.clone()),
            name: row.get("name"),
            personality: row.get("personality"),
            scenario: row.get("scenario"),
            system_prompt: row.get("system_prompt"),
            example_dialogue: row.get("example_dialogue"),
            appearance: row.get("appearance"),
            tags: decode(row.get::<&[u8], _>("tags")).unwrap_or_default(),
            avatar: row.get("avatar"),
            is_public: row.get("is_public"),
            owned_by_user: row.get::<String, _>("owner_id") == user_id,
        });
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "character".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?))
            .await?;
    }
    drop(characters);
    let mut conversations=sqlx::query("SELECT c.id,c.title,c.kind,CAST(c.state AS TEXT) AS state,c.summary,c.revision,(SELECT json_group_array(character_id) FROM (SELECT character_id FROM participants WHERE conversation_id=c.id ORDER BY position)) AS participant_ids FROM conversations c WHERE c.owner_id=? AND c.revision<=? ORDER BY c.id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = conversations.try_next().await? {
        let entity_id: String = row.get("id");
        let participant_ids: Vec<String> =
            serde_json::from_str(row.get::<&str, _>("participant_ids"))?;
        let payload = DeltaPayload::Conversation {
            title: row.get("title"),
            kind: ConversationKind::try_from(row.get::<i32, _>("kind"))?,
            participant_ids,
            state: row.get("state"),
            summary: row.get("summary"),
        };
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "conversation".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?))
            .await?;
    }
    drop(conversations);
    let mut messages=sqlx::query("SELECT m.id,m.conversation_id,m.author_type,m.author_id,m.content,m.parent_id,m.selected_variant_id,m.revision FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE c.owner_id=? AND m.revision<=? ORDER BY m.id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = messages.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Message {
            conversation_id: row.get("conversation_id"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
        };
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "message".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?))
            .await?;
    }
    drop(messages);
    let mut lore=sqlx::query("SELECT id,conversation_id,keywords,content,always_on,priority,revision FROM lore WHERE owner_id=? AND revision<=? ORDER BY id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = lore.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Lore(LoreInput {
            id: Some(entity_id.clone()),
            conversation_id: row.get("conversation_id"),
            keywords: decode(row.get::<&[u8], _>("keywords")).unwrap_or_default(),
            content: row.get("content"),
            always_on: row.get("always_on"),
            priority: row.get("priority"),
        });
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "lore".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?))
            .await?;
    }
    drop(lore);
    let mut memories=sqlx::query("SELECT id,conversation_id,character_id,content,revision FROM memories WHERE owner_id=? AND revision<=? ORDER BY id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = memories.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Memory(MemoryInput {
            id: Some(entity_id.clone()),
            conversation_id: row.get("conversation_id"),
            character_id: row.get("character_id"),
            content: row.get("content"),
        });
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "memory".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?))
            .await?;
    }
    tx.send((
        MessageType::Response,
        request_id,
        encode(&Response::SyncComplete {
            revision: snapshot_revision,
        })?,
    ))
    .await?;
    Ok(())
}

async fn extract_memory(app: &App, conversation_id: &str) -> Result<String> {
    let models = probe_backend(app).await?;
    let model = models.first().context("no model loaded")?;
    let recent: Vec<String> = sqlx::query_scalar(
        "SELECT author_type || ': ' || COALESCE(v.content,m.content) FROM messages m LEFT JOIN variants v ON v.id=m.selected_variant_id AND v.message_id=m.id WHERE m.conversation_id=? AND m.parent_id IS NULL ORDER BY m.created_at DESC LIMIT 40",
    )
    .bind(conversation_id)
    .fetch_all(&app.db)
    .await?;
    if recent.is_empty() {
        bail!("conversation has no history to extract")
    }
    let response = app
        .http
        .post(format!("{}/chat/completions", adapter_url(app).await?))
        .timeout(Duration::from_secs(30))
        .json(&json!({
            "model": model,
            "messages": [
                {"role":"system","content":"Extract exactly one durable roleplay fact worth remembering from the transcript. Return only the fact as one concise sentence. Do not add labels, markdown, instructions, guesses, or private reasoning. If there is no durable fact, return NONE."},
                {"role":"user","content":recent.into_iter().rev().collect::<Vec<_>>().join("\n")}
            ],
            "stream": false,
            "max_tokens": 128,
            "temperature": 0.1
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    validate_extracted_memory(
        response["choices"][0]["message"]["content"]
            .as_str()
            .context("memory extraction response missing content")?,
    )
}

fn fallback_chat_title(message: &str) -> String {
    let title = message
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .trim();
    if title.is_empty() {
        "New chat".into()
    } else {
        clip(title, 80)
    }
}

fn clean_chat_title(candidate: &str, fallback: &str) -> String {
    let title = candidate
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '#' | '*' | '.'))
        .trim();
    if title.is_empty() || title.eq_ignore_ascii_case("new chat") {
        fallback_chat_title(fallback)
    } else {
        clip(title, 80)
    }
}

async fn maybe_name_new_chat(
    app: &App,
    user_id: &str,
    conversation_id: &str,
    first_message: &str,
) -> Result<Option<StateDelta>> {
    let row = sqlx::query(
        "SELECT title,kind,CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=? AND owner_id=?",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(&app.db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    if row.get::<String, _>("title") != "New chat" {
        return Ok(None);
    }
    let user_messages: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE conversation_id=? AND author_type='user' AND parent_id IS NULL",
    )
    .bind(conversation_id)
    .fetch_one(&app.db)
    .await?;
    if user_messages != 1 {
        return Ok(None);
    }

    let fallback = fallback_chat_title(first_message);
    let generated = async {
        let models = probe_backend(app).await.ok()?;
        let model = models.first()?;
        let response = app
            .http
            .post(format!("{}/chat/completions", adapter_url(app).await.ok()?))
            .timeout(Duration::from_secs(12))
            .json(&json!({
                "model": model,
                "messages": [
                    {"role":"system","content":"Name this conversation in 2 to 6 words. Return only the title, without quotes or punctuation."},
                    {"role":"user","content": first_message}
                ],
                "stream": false,
                "temperature": 0.2,
                "max_tokens": 24
            }))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        response["choices"][0]["message"]["content"]
            .as_str()
            .map(|title| clean_chat_title(title, first_message))
    }
    .await;
    let title = generated.unwrap_or(fallback);
    let kind = ConversationKind::try_from(row.get::<i32, _>("kind"))?;
    let participant_ids: Vec<String> = sqlx::query_scalar(
        "SELECT character_id FROM participants WHERE conversation_id=? ORDER BY position",
    )
    .bind(conversation_id)
    .fetch_all(&app.db)
    .await?;
    let changed = encode(&DeltaPayload::Conversation {
        title: title.clone(),
        kind,
        participant_ids,
        state: row.get("state"),
        summary: row.get("summary"),
    })?;
    let mut transaction = app.db.begin().await?;
    let still_unnamed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=? AND title='New chat')",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !still_unnamed {
        transaction.rollback().await?;
        return Ok(None);
    }
    let revision = delta_tx(
        &mut transaction,
        user_id,
        "conversation",
        conversation_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE conversations SET title=?,revision=? WHERE id=? AND owner_id=?")
        .bind(title)
        .bind(revision)
        .bind(conversation_id)
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(Some(StateDelta {
        revision,
        entity_type: "conversation".into(),
        entity_id: conversation_id.into(),
        operation: DeltaOperation::Update,
        changed_fields: changed,
    }))
}

fn validate_extracted_memory(value: &str) -> Result<String> {
    let content = value.trim();
    if content.eq_ignore_ascii_case("none") || content.is_empty() {
        bail!("model found no durable memory")
    }
    if content.chars().count() > 1024 || content.contains("```\n") {
        bail!("model returned an invalid memory")
    }
    Ok(content.to_owned())
}

async fn probe_backend(app: &App) -> Result<Vec<String>> {
    let v: Value = app
        .http
        .get(format!("{}/models", adapter_url(app).await?))
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v["data"]
        .as_array()
        .context("models response missing data")?
        .iter()
        .filter_map(|x| x["id"].as_str().map(str::to_owned))
        .collect())
}

async fn generate(app: &App, job: Generation<'_>) -> Result<()> {
    let Generation {
        tx,
        request_id: req_id,
        user_id: uid,
        conversation_id: cid,
        speaker_id: speaker,
        parent_id: parent,
        mut cancel,
        origin,
    } = job;
    let models = probe_backend(app).await?;
    let model = models.first().context("no model loaded")?;
    let participants=sqlx::query("SELECT c.id,c.name,c.system_prompt,c.personality,c.scenario,c.appearance,c.example_dialogue FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? ORDER BY p.position").bind(cid).fetch_all(&app.db).await?;
    let sid = select_speaker(app, cid, speaker, &participants).await?;
    let character = participants
        .iter()
        .find(|r| r.get::<String, _>("id") == sid)
        .context("speaker is not a participant")?;
    let recent=sqlx::query("SELECT m.author_type,m.author_id,COALESCE(v.content,m.content) AS content FROM messages m LEFT JOIN variants v ON v.id=m.selected_variant_id AND v.message_id=m.id WHERE m.conversation_id=? AND m.parent_id IS NULL ORDER BY m.created_at DESC LIMIT 80").bind(cid).fetch_all(&app.db).await?;
    let joined = recent
        .iter()
        .map(|r| clip(&r.get::<String, _>("content"), 4096))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let lore=sqlx::query("SELECT keywords,content,always_on FROM lore WHERE owner_id=? AND (conversation_id IS NULL OR conversation_id=?) ORDER BY priority DESC LIMIT 64").bind(uid).bind(cid).fetch_all(&app.db).await?.into_iter().filter_map(|r|{let keys:Vec<String>=decode(r.get::<&[u8],_>("keywords")).unwrap_or_default();if r.get::<bool,_>("always_on")||keys.iter().any(|k|joined.contains(&k.to_lowercase())){Some(clip(&r.get::<String,_>("content"),4096))}else{None}}).collect::<Vec<_>>();
    let memories:Vec<String>=sqlx::query_scalar("SELECT content FROM memories WHERE owner_id=? AND (conversation_id IS NULL OR conversation_id=?) AND (character_id IS NULL OR character_id=?) LIMIT 64").bind(uid).bind(cid).bind(&sid).fetch_all(&app.db).await?;
    let context =
        sqlx::query("SELECT CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=?")
            .bind(cid)
            .fetch_one(&app.db)
            .await?;
    let system = format!(
        "{}\nYou are {}. Personality: {}\nAppearance: {}\nScenario: {}\nExample dialogue:\n{}\nGroup participants:\n{}\nWorld state:\n{}\nStory summary:\n{}\nLore:\n{}\nMemory:\n{}",
        clip(&character.get::<String, _>("system_prompt"), 16_384),
        character.get::<String, _>("name"),
        clip(&character.get::<String, _>("personality"), 16_384),
        clip(&character.get::<String, _>("appearance"), 16_384),
        clip(&character.get::<String, _>("scenario"), 16_384),
        clip(&character.get::<String, _>("example_dialogue"), 16_384),
        participants
            .iter()
            .map(|r| format!(
                "{} — personality: {}; appearance: {}; scenario: {}",
                r.get::<String, _>("name"),
                clip(&r.get::<String, _>("personality"), 512),
                clip(&r.get::<String, _>("appearance"), 512),
                clip(&r.get::<String, _>("scenario"), 512)
            ))
            .collect::<Vec<_>>()
            .join(", "),
        clip(&context.get::<String, _>("state"), 32_768),
        clip(&context.get::<String, _>("summary"), 32_768),
        lore.join("\n"),
        memories
            .iter()
            .map(|memory| clip(memory, 4096))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut messages = vec![json!({"role":"system","content":system})];
    for r in recent.into_iter().rev() {
        let author_type = r.get::<String, _>("author_type");
        let role = match author_type.as_str() {
            "user" => "user",
            "system" => "system",
            _ => "assistant",
        };
        messages.push(json!({"role":role,"content":clip(&r.get::<String,_>("content"),8192)}));
    }
    let mid = Uuid::new_v4().to_string();
    tx.send((
        MessageType::Response,
        req_id,
        encode(&Response::GenerationStarted {
            message_id: mid.clone(),
            character_id: sid.clone(),
        })?,
    ))
    .await?;
    let response_future = app
        .http
        .post(format!("{}/chat/completions", adapter_url(app).await?))
        .json(&json!({"model":model,"messages":messages,"stream":true}))
        .send();
    let response=tokio::select!{biased;
        changed=cancel.changed()=>{
            if changed.is_err()||*cancel.borrow(){
                let revision:i64=sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas WHERE owner_id=?").bind(uid).fetch_one(&app.db).await?;
                tx.send((MessageType::StreamEnd,req_id,encode(&Response::GenerationFinished{message_id:mid,revision,cancelled:true})?)).await?;
                return Ok(());
            }
            bail!("generation cancellation channel changed unexpectedly")
        }
        response=response_future=>response?
    }.error_for_status()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream")
    {
        bail!("backend does not support streaming SSE (content-type: {content_type})")
    }
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut complete = String::new();
    let mut sse_buffer = Vec::new();
    let mut seq = 0;
    let mut deadline = Instant::now() + Duration::from_millis(60);
    let mut cancelled = false;
    let mut done = false;
    loop {
        enum Event<T> {
            Cancel,
            Timeout,
            Data(Option<T>),
        }
        let event = tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { Event::Cancel } else { continue }
            }
            _ = tokio::time::sleep_until(deadline) => Event::Timeout,
            item = stream.next() => Event::Data(item),
        };
        match event {
            Event::Cancel => {
                cancelled = true;
                break;
            }
            Event::Timeout => {}
            Event::Data(Some(chunk)) => {
                sse_buffer.extend_from_slice(&chunk?);
                done = drain_sse(&mut sse_buffer, &mut pending, &mut complete)?;
            }
            Event::Data(None) => {
                if !sse_buffer.is_empty() {
                    sse_buffer.push(b'\n');
                    let _ = drain_sse(&mut sse_buffer, &mut pending, &mut complete)?;
                }
                break;
            }
        }
        if !pending.is_empty()
            && (pending.split_whitespace().count() >= 32 || Instant::now() >= deadline || done)
        {
            seq += 1;
            let out = StreamChunk {
                message_id: mid.clone(),
                sequence: seq,
                text: std::mem::take(&mut pending),
            };
            tx.send((MessageType::StreamChunk, req_id, encode(&out)?))
                .await?;
            deadline = Instant::now() + Duration::from_millis(60);
        } else if Instant::now() >= deadline {
            deadline = Instant::now() + Duration::from_millis(60);
        }
        if done {
            break;
        }
    }
    if !pending.is_empty() {
        seq += 1;
        tx.send((
            MessageType::StreamChunk,
            req_id,
            encode(&StreamChunk {
                message_id: mid.clone(),
                sequence: seq,
                text: pending,
            })?,
        ))
        .await?;
    }
    let delta = persist_generation(app, uid, cid, &sid, &mid, &complete, parent.as_deref()).await?;
    let rev = delta.revision;
    tx.send((MessageType::Delta, req_id, encode(&delta)?))
        .await?;
    let _ = app.deltas.send(PublishedDelta {
        owner_id: uid.into(),
        origin,
        delta,
    });
    tx.send((
        MessageType::StreamEnd,
        req_id,
        encode(&Response::GenerationFinished {
            message_id: mid,
            revision: rev,
            cancelled,
        })?,
    ))
    .await?;
    Ok(())
}

async fn persist_generation(
    app: &App,
    user_id: &str,
    conversation_id: &str,
    speaker_id: &str,
    generation_id: &str,
    content: &str,
    parent_id: Option<&str>,
) -> Result<StateDelta> {
    let is_variant = parent_id.is_some();
    let changed = if let Some(parent_id) = parent_id {
        encode(&DeltaPayload::Variant {
            message_id: parent_id.into(),
            content: content.into(),
        })?
    } else {
        encode(&DeltaPayload::Message {
            conversation_id: conversation_id.into(),
            author_type: "character".into(),
            author_id: Some(speaker_id.into()),
            content: content.into(),
            parent_id: None,
            selected_variant_id: None,
        })?
    };
    let mut transaction = app.db.begin().await?;
    let revision = delta_tx(
        &mut transaction,
        user_id,
        if is_variant { "variant" } else { "message" },
        generation_id,
        DeltaOperation::Add,
        &changed,
    )
    .await?;
    if let Some(parent_id) = parent_id {
        sqlx::query("INSERT INTO variants(id,message_id,content,revision) VALUES(?,?,?,?)")
            .bind(generation_id)
            .bind(parent_id)
            .bind(content)
            .bind(revision)
            .execute(&mut *transaction)
            .await?;
        let updated = sqlx::query(
            "UPDATE messages SET selected_variant_id=?,revision=? WHERE id=? AND conversation_id=?",
        )
        .bind(generation_id)
        .bind(revision)
        .bind(parent_id)
        .bind(conversation_id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            bail!("variant parent disappeared during generation")
        }
    } else {
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,parent_id,revision) VALUES(?,?,?,?,?,NULL,?)")
            .bind(generation_id)
            .bind(conversation_id)
            .bind("character")
            .bind(speaker_id)
            .bind(content)
            .bind(revision)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(StateDelta {
        revision,
        entity_type: if is_variant {
            "variant".into()
        } else {
            "message".into()
        },
        entity_id: generation_id.into(),
        operation: DeltaOperation::Add,
        changed_fields: changed,
    })
}

fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn drain_sse(buffer: &mut Vec<u8>, pending: &mut String, complete: &mut String) -> Result<bool> {
    let mut consumed = 0;
    let mut done = false;
    while let Some(relative_end) = buffer[consumed..].iter().position(|b| *b == b'\n') {
        let end = consumed + relative_end;
        let line = std::str::from_utf8(&buffer[consumed..end])?.trim_end_matches('\r');
        consumed = end + 1;
        let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
            continue;
        };
        if data == "[DONE]" {
            done = true;
            break;
        }
        let value: Value =
            serde_json::from_str(data).context("malformed llama-server SSE event")?;
        if let Some(text) = value["choices"][0]["delta"]["content"].as_str() {
            pending.push_str(text);
            complete.push_str(text);
        }
    }
    buffer.drain(..consumed);
    Ok(done)
}

async fn select_speaker(
    app: &App,
    cid: &str,
    explicit: Option<String>,
    participants: &[sqlx::sqlite::SqliteRow],
) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    if participants.is_empty() {
        bail!("conversation has no character")
    }
    let row = sqlx::query("SELECT kind,turn_index FROM conversations WHERE id=?")
        .bind(cid)
        .fetch_one(&app.db)
        .await?;
    let kind: i32 = row.get("kind");
    let turn: i64 = row.get("turn_index");
    let fallback = participants[turn as usize % participants.len()].get::<String, _>("id");
    if kind == ConversationKind::GroupManual as i32 {
        bail!("manual group mode requires an explicit speaker")
    }
    let selected = if kind == ConversationKind::GroupAutomatic as i32 {
        let names = participants
            .iter()
            .map(|r| r.get::<String, _>("name"))
            .collect::<Vec<_>>();
        let recent:Vec<String>=sqlx::query_scalar("SELECT content FROM messages WHERE conversation_id=? AND parent_id IS NULL ORDER BY created_at DESC LIMIT 12").bind(cid).fetch_all(&app.db).await.unwrap_or_default();
        let choice = async {
            let models = probe_backend(app).await.ok()?;
            let model = models.first()?;
            let response=app.http.post(format!("{}/chat/completions",adapter_url(app).await.ok()?)).timeout(Duration::from_secs(15)).json(&json!({"model":model,"messages":[{"role":"system","content":"Choose exactly one next speaker name from the supplied list based on the recent roleplay. Output only the name."},{"role":"user","content":format!("Speakers: {}\nRecent roleplay:\n{}",names.join(", "),recent.into_iter().rev().collect::<Vec<_>>().join("\n"))}],"stream":false,"max_tokens":16})).send().await.ok()?.error_for_status().ok()?.json::<Value>().await.ok()?;
            let answer = response["choices"][0]["message"]["content"].as_str()?.trim();
            participants.iter().find(|r|r.get::<String,_>("name").eq_ignore_ascii_case(answer)).map(|r|r.get("id"))
        }.await;
        choice.unwrap_or(fallback)
    } else {
        fallback
    };
    sqlx::query("UPDATE conversations SET turn_index=turn_index+1 WHERE id=?")
        .bind(cid)
        .execute(&app.db)
        .await?;
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_uses_absolute_xdg_data_home() {
        assert_eq!(
            default_user_data_dir(
                Some(PathBuf::from("/tmp/xdg-data")),
                Some(PathBuf::from("/home/tester")),
            ),
            Some(PathBuf::from("/tmp/xdg-data/chatty")),
        );
    }

    #[test]
    fn database_uses_linux_home_fallback() {
        assert_eq!(
            default_user_data_dir(None, Some(PathBuf::from("/home/tester"))),
            Some(PathBuf::from("/home/tester/.local/share/chatty")),
        );
    }

    #[test]
    fn database_never_falls_back_to_current_directory() {
        assert_eq!(default_user_data_dir(None, None), None);
        assert_eq!(
            default_user_data_dir(
                Some(PathBuf::from("relative/data")),
                Some(PathBuf::from("relative/home")),
            ),
            None,
        );
    }

    #[test]
    fn fragmented_sse_is_buffered_without_data_loss() {
        let mut buffer = br#"data: {"choices":[{"delta":{"cont"#.to_vec();
        let mut pending = String::new();
        let mut complete = String::new();
        assert!(!drain_sse(&mut buffer, &mut pending, &mut complete).unwrap());
        buffer.extend_from_slice(br#"ent":"hello"}}]}"#);
        buffer.extend_from_slice(b"\n\ndata: [DONE]\n\n");
        assert!(drain_sse(&mut buffer, &mut pending, &mut complete).unwrap());
        assert_eq!(pending, "hello");
        assert_eq!(complete, "hello");
    }

    #[test]
    fn live_delta_visibility_is_owner_and_origin_scoped() {
        let origin = Uuid::new_v4();
        let peer = Uuid::new_v4();
        let event = PublishedDelta {
            owner_id: "owner-a".into(),
            origin,
            delta: StateDelta {
                revision: 1,
                entity_type: "memory".into(),
                entity_id: "m1".into(),
                operation: DeltaOperation::Add,
                changed_fields: vec![],
            },
        };
        assert!(delta_visible(Some("owner-a"), peer, &event));
        assert!(!delta_visible(Some("owner-b"), peer, &event));
        assert!(!delta_visible(None, peer, &event));
        assert!(!delta_visible(Some("owner-a"), origin, &event));
    }

    #[test]
    fn generated_chat_titles_are_clean_and_bounded() {
        assert_eq!(
            clean_chat_title("\"A Northern Journey.\"\nignored", "fallback"),
            "A Northern Journey"
        );
        assert_eq!(
            fallback_chat_title("  Plan a journey over the northern road tomorrow please  "),
            "Plan a journey over the northern road"
        );
        assert_eq!(fallback_chat_title("..."), "New chat");
    }

    #[test]
    fn extracted_memory_is_bounded_and_rejects_empty_results() {
        assert_eq!(
            validate_extracted_memory("  Rowan promised to guard the gate.  ").unwrap(),
            "Rowan promised to guard the gate."
        );
        assert!(validate_extracted_memory("NONE").is_err());
        assert!(validate_extracted_memory("").is_err());
        assert!(validate_extracted_memory(&"x".repeat(1025)).is_err());
    }

    async fn call(app: &App, request: Request) -> Result<(MessageType, Vec<u8>)> {
        let (tx, mut rx) = mpsc::channel(32);
        let dispatch_app = app.clone();
        let task = tokio::spawn(async move {
            dispatch(
                dispatch_app,
                tx,
                Arc::new(RwLock::new(None)),
                Uuid::new_v4(),
                1,
                request,
            )
            .await
        });
        let mut result = None;
        while let Some((kind, _, payload)) = rx.recv().await {
            if matches!(kind, MessageType::Response | MessageType::Error) {
                result = Some((kind, payload));
                break;
            }
        }
        task.await??;
        result.context("missing response")
    }

    #[tokio::test]
    async fn saved_session_resume_does_not_depend_on_adapter() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        sqlx::query("INSERT INTO broker_settings(singleton,adapter_enabled,adapter_url) VALUES(1,0,'http://127.0.0.1:1/v1')")
            .execute(&db)
            .await
            .unwrap();
        let (deltas, _) = broadcast::channel(32);
        let app = App {
            db,
            http: reqwest::Client::new(),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            snapshot_gate: Arc::new(RwLock::new(())),
            deltas,
            recent_errors: Arc::new(Mutex::new(Vec::new())),
        };
        let (_, registered) = call(
            &app,
            Request::Register {
                username: "resume-user".into(),
                password: "resume-password".into(),
            },
        )
        .await
        .unwrap();
        let token = match decode::<Response>(&registered).unwrap() {
            Response::Authenticated { session_token, .. } => session_token,
            other => panic!("unexpected response: {other:?}"),
        };

        let (tx, mut rx) = mpsc::channel(32);
        let resume_app = app.clone();
        let resume = tokio::spawn(async move {
            dispatch(
                resume_app,
                tx,
                Arc::new(RwLock::new(None)),
                Uuid::new_v4(),
                2,
                Request::Resume {
                    session_token: token,
                    since_revision: 0,
                },
            )
            .await
        });
        let mut authenticated = false;
        let mut synchronized = false;
        while let Some((kind, _, payload)) = rx.recv().await {
            if kind != MessageType::Response {
                continue;
            }
            match decode::<Response>(&payload).unwrap() {
                Response::Authenticated { role, .. } => {
                    authenticated = true;
                    assert_eq!(role, Role::Admin);
                }
                Response::SyncComplete { .. } => {
                    synchronized = true;
                    break;
                }
                _ => {}
            }
        }
        resume.await.unwrap().unwrap();
        assert!(authenticated);
        assert!(synchronized);
        let monitor = broker_monitor(&app).await;
        assert_eq!(monitor.adapter_status, AdapterStatus::Disabled);
        assert_eq!(monitor.adapter_model_count, 0);
    }

    #[tokio::test]
    async fn cross_tenant_character_update_is_forbidden() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        sqlx::query(
            "INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,'http://127.0.0.1:1/v1')",
        )
        .execute(&db)
        .await
        .unwrap();
        let (deltas, _) = broadcast::channel(32);
        let app = App {
            db,
            http: reqwest::Client::new(),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            snapshot_gate: Arc::new(RwLock::new(())),
            deltas,
            recent_errors: Arc::new(Mutex::new(Vec::new())),
        };
        let (_, first) = call(
            &app,
            Request::Register {
                username: "first-user".into(),
                password: "first-password".into(),
            },
        )
        .await
        .unwrap();
        let first_token = match decode::<Response>(&first).unwrap() {
            Response::Authenticated { session_token, .. } => session_token,
            other => panic!("unexpected response: {other:?}"),
        };
        let (_, default_chat) = call(
            &app,
            Request::CreateConversation {
                session_token: first_token.clone(),
                title: "New chat".into(),
                kind: ConversationKind::Direct,
                participant_ids: vec![],
            },
        )
        .await
        .unwrap();
        let default_chat_id = match decode::<Response>(&default_chat).unwrap() {
            Response::Accepted {
                entity_id: Some(id),
                ..
            } => id,
            other => panic!("unexpected response: {other:?}"),
        };
        let default_name: String = sqlx::query_scalar(
            "SELECT c.name FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=?",
        )
        .bind(&default_chat_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(default_name, "Assistant");
        let owner_id: String = sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=?")
            .bind(&first_token)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let assistant_id: String =
            sqlx::query_scalar("SELECT character_id FROM participants WHERE conversation_id=?")
                .bind(&default_chat_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        persist_generation(
            &app,
            &owner_id,
            &default_chat_id,
            &assistant_id,
            "original-response",
            "The original response",
            None,
        )
        .await
        .unwrap();
        for (id, content) in [
            ("variant-one", "First retry"),
            ("variant-two", "Second retry"),
        ] {
            persist_generation(
                &app,
                &owner_id,
                &default_chat_id,
                &assistant_id,
                id,
                content,
                Some("original-response"),
            )
            .await
            .unwrap();
        }
        let view = load_conversation(&app.db, &default_chat_id).await.unwrap();
        let response = view
            .messages
            .iter()
            .find(|message| message.id == "original-response")
            .unwrap();
        assert_eq!(response.content, "The original response");
        assert_eq!(response.variants.len(), 2);
        assert_eq!(response.selected_variant_id.as_deref(), Some("variant-two"));
        assert_eq!(
            view.messages
                .iter()
                .filter(|message| message.parent_id.is_some())
                .count(),
            0
        );
        call(
            &app,
            Request::SelectVariant {
                session_token: first_token.clone(),
                message_id: "original-response".into(),
                variant_id: "original-response".into(),
            },
        )
        .await
        .unwrap();
        let selected: Option<String> =
            sqlx::query_scalar("SELECT selected_variant_id FROM messages WHERE id=?")
                .bind("original-response")
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(selected.is_none());
        call(
            &app,
            Request::SendMessage {
                session_token: first_token.clone(),
                conversation_id: default_chat_id.clone(),
                content: "Plan a journey over the northern road tomorrow please".into(),
                speaker_id: None,
            },
        )
        .await
        .unwrap();
        let default_title: String =
            sqlx::query_scalar("SELECT title FROM conversations WHERE id=?")
                .bind(&default_chat_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(default_title, "Plan a journey over the northern road");
        let character = CharacterInput {
            id: None,
            name: "Owner's character".into(),
            personality: String::new(),
            scenario: String::new(),
            system_prompt: String::new(),
            example_dialogue: String::new(),
            appearance: String::new(),
            tags: vec![],
            avatar: None,
            is_public: false,
            owned_by_user: true,
        };
        let (_, created) = call(
            &app,
            Request::UpsertCharacter {
                session_token: first_token.clone(),
                character: character.clone(),
            },
        )
        .await
        .unwrap();
        let character_id = match decode::<Response>(&created).unwrap() {
            Response::Accepted {
                entity_id: Some(id),
                ..
            } => id,
            other => panic!("unexpected response: {other:?}"),
        };
        let first_delta=sqlx::query("SELECT operation,changed_fields FROM deltas WHERE entity_id=? ORDER BY revision DESC LIMIT 1").bind(&character_id).fetch_one(&app.db).await.unwrap();
        assert_eq!(first_delta.get::<i32, _>("operation"), 0);
        assert!(matches!(
            decode::<DeltaPayload>(first_delta.get::<&[u8], _>("changed_fields")).unwrap(),
            DeltaPayload::Character(_)
        ));
        let mut updated = character.clone();
        updated.id = Some(character_id.clone());
        updated.is_public = true;
        call(
            &app,
            Request::UpsertCharacter {
                session_token: first_token.clone(),
                character: updated,
            },
        )
        .await
        .unwrap();
        let update_operation: i32 = sqlx::query_scalar(
            "SELECT operation FROM deltas WHERE entity_id=? ORDER BY revision DESC LIMIT 1",
        )
        .bind(&character_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(update_operation, 1);
        for (kind, should_select) in [
            (ConversationKind::GroupRoundRobin, true),
            (ConversationKind::GroupAutomatic, true),
            (ConversationKind::GroupManual, false),
        ] {
            let (_, created) = call(
                &app,
                Request::CreateConversation {
                    session_token: first_token.clone(),
                    title: "group".into(),
                    kind,
                    participant_ids: vec![character_id.clone()],
                },
            )
            .await
            .unwrap();
            let conversation_id = match decode::<Response>(&created).unwrap() {
                Response::Accepted {
                    entity_id: Some(id),
                    ..
                } => id,
                other => panic!("unexpected response: {other:?}"),
            };
            let participants=sqlx::query("SELECT c.id,c.name,c.system_prompt,c.personality,c.scenario,c.appearance,c.example_dialogue FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? ORDER BY p.position").bind(&conversation_id).fetch_all(&app.db).await.unwrap();
            let selection = select_speaker(&app, &conversation_id, None, &participants).await;
            if should_select {
                assert_eq!(selection.unwrap(), character_id);
            } else {
                assert!(
                    selection
                        .unwrap_err()
                        .to_string()
                        .contains("explicit speaker")
                );
            }
        }
        let (_, created) = call(
            &app,
            Request::CreateConversation {
                session_token: first_token.clone(),
                title: "cascade".into(),
                kind: ConversationKind::Direct,
                participant_ids: vec![character_id.clone()],
            },
        )
        .await
        .unwrap();
        let cascade_id = match decode::<Response>(&created).unwrap() {
            Response::Accepted {
                entity_id: Some(id),
                ..
            } => id,
            other => panic!("unexpected response: {other:?}"),
        };
        call(
            &app,
            Request::UpdateConversation {
                session_token: first_token.clone(),
                conversation_id: cascade_id.clone(),
                title: "renamed cascade".into(),
                participant_ids: vec![character_id.clone()],
            },
        )
        .await
        .unwrap();
        let renamed: String = sqlx::query_scalar("SELECT title FROM conversations WHERE id=?")
            .bind(&cascade_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(renamed, "renamed cascade");
        call(
            &app,
            Request::SendMessage {
                session_token: first_token.clone(),
                conversation_id: cascade_id.clone(),
                content: "persisted".into(),
                speaker_id: None,
            },
        )
        .await
        .unwrap();
        call(
            &app,
            Request::UpsertLore {
                session_token: first_token.clone(),
                lore: LoreInput {
                    id: None,
                    conversation_id: Some(cascade_id.clone()),
                    keywords: vec!["key".into()],
                    content: "lore".into(),
                    always_on: false,
                    priority: 1,
                },
            },
        )
        .await
        .unwrap();
        call(
            &app,
            Request::UpsertMemory {
                session_token: first_token.clone(),
                memory: MemoryInput {
                    id: None,
                    conversation_id: Some(cascade_id.clone()),
                    character_id: None,
                    content: "memory".into(),
                },
            },
        )
        .await
        .unwrap();
        call(
            &app,
            Request::DeleteEntity {
                session_token: first_token.clone(),
                kind: EntityKind::Conversation,
                entity_id: cascade_id.clone(),
            },
        )
        .await
        .unwrap();
        for table in ["conversations", "messages", "lore", "memories"] {
            let sql = format!(
                "SELECT COUNT(*) FROM {table} WHERE {}=?",
                if table == "conversations" {
                    "id"
                } else {
                    "conversation_id"
                }
            );
            let count: i64 = sqlx::query_scalar(&sql)
                .bind(&cascade_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
            assert_eq!(count, 0, "{table} was not cascade-deleted");
        }
        let delete_types:Vec<String>=sqlx::query_scalar("SELECT entity_type FROM deltas WHERE operation=2 AND owner_id=(SELECT id FROM users WHERE username='first-user')").fetch_all(&app.db).await.unwrap();
        for expected in ["conversation", "message", "lore", "memory"] {
            assert!(
                delete_types.iter().any(|kind| kind == expected),
                "missing {expected} delete delta"
            );
        }
        let (snapshot_tx, mut snapshot_rx) = mpsc::channel(64);
        let snapshot_app = app.clone();
        let snapshot_token = first_token.clone();
        let snapshot = tokio::spawn(async move {
            dispatch(
                snapshot_app,
                snapshot_tx,
                Arc::new(RwLock::new(None)),
                Uuid::new_v4(),
                77,
                Request::Snapshot {
                    session_token: snapshot_token,
                },
            )
            .await
        });
        let mut snapshot_entities = HashMap::new();
        while let Some((kind, _, payload)) = snapshot_rx.recv().await {
            match kind {
                MessageType::Delta => {
                    let delta: StateDelta = decode(&payload).unwrap();
                    snapshot_entities.insert(
                        (delta.entity_type.clone(), delta.entity_id.clone()),
                        decode::<DeltaPayload>(&delta.changed_fields).unwrap(),
                    );
                }
                MessageType::Response => {
                    assert!(matches!(
                        decode::<Response>(&payload).unwrap(),
                        Response::SyncComplete { .. }
                    ));
                    break;
                }
                other => panic!("unexpected snapshot frame: {other:?}"),
            }
        }
        snapshot.await.unwrap().unwrap();
        assert!(snapshot_entities.contains_key(&("character".into(), character_id.clone())));
        assert!(!snapshot_entities.contains_key(&("conversation".into(), cascade_id)));
        let (_, second) = call(
            &app,
            Request::Register {
                username: "second-user".into(),
                password: "second-password".into(),
            },
        )
        .await
        .unwrap();
        let second_token = match decode::<Response>(&second).unwrap() {
            Response::Authenticated { session_token, .. } => session_token,
            other => panic!("unexpected response: {other:?}"),
        };
        let (_, listed) = call(
            &app,
            Request::ListCharacters {
                session_token: second_token.clone(),
            },
        )
        .await
        .unwrap();
        let shared = match decode::<Response>(&listed).unwrap() {
            Response::Characters(characters) => characters
                .into_iter()
                .find(|candidate| candidate.id == character_id)
                .expect("public character should be visible"),
            other => panic!("unexpected response: {other:?}"),
        };
        assert!(shared.is_public);
        assert!(!shared.owned_by_user);
        call(
            &app,
            Request::CreateConversation {
                session_token: second_token.clone(),
                title: "Shared character chat".into(),
                kind: ConversationKind::Direct,
                participant_ids: vec![character_id.clone()],
            },
        )
        .await
        .unwrap();
        call(
            &app,
            Request::AdminSetBrokerConfig {
                session_token: first_token.clone(),
                config: BrokerConfig {
                    adapter_enabled: false,
                    adapter_url: "http://127.0.0.1:11434/v1".into(),
                    allow_public_characters: false,
                    allow_self_registration: false,
                },
            },
        )
        .await
        .unwrap();
        let (_, capabilities) = call(&app, Request::GetServerCapabilities).await.unwrap();
        assert!(matches!(
            decode::<Response>(&capabilities).unwrap(),
            Response::ServerCapabilities {
                registration_enabled: false
            }
        ));
        call(
            &app,
            Request::AdminCreateUser {
                session_token: first_token.clone(),
                username: "managed-user".into(),
                password: "managed-password".into(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        let managed_role: String =
            sqlx::query_scalar("SELECT role FROM users WHERE username='managed-user'")
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(managed_role, "user");
        let self_delete_error = call(
            &app,
            Request::AdminDeleteUser {
                session_token: first_token.clone(),
                user_id: owner_id.clone(),
            },
        )
        .await
        .unwrap_err();
        assert!(self_delete_error.to_string().contains("own active account"));
        let managed_id: String =
            sqlx::query_scalar("SELECT id FROM users WHERE username='managed-user'")
                .fetch_one(&app.db)
                .await
                .unwrap();
        call(
            &app,
            Request::AdminDeleteUser {
                session_token: first_token.clone(),
                user_id: managed_id,
            },
        )
        .await
        .unwrap();
        let managed_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username='managed-user')")
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(!managed_exists);
        let registration_error = call(
            &app,
            Request::Register {
                username: "blocked-user".into(),
                password: "blocked-password".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(registration_error.to_string().contains("registration"));
        let mut blocked_public = character.clone();
        blocked_public.name = "Blocked public character".into();
        blocked_public.is_public = true;
        let publishing_error = call(
            &app,
            Request::UpsertCharacter {
                session_token: second_token.clone(),
                character: blocked_public,
            },
        )
        .await
        .unwrap_err();
        assert!(publishing_error.to_string().contains("publishing"));
        call(
            &app,
            Request::AdminSetCharacterPublic {
                session_token: first_token.clone(),
                character_id: character_id.clone(),
                is_public: false,
            },
        )
        .await
        .unwrap();
        let is_public: bool = sqlx::query_scalar("SELECT is_public FROM characters WHERE id=?")
            .bind(&character_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(!is_public);
        let (_, database) = call(
            &app,
            Request::AdminReadDatabase {
                session_token: first_token,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            decode::<Response>(&database).unwrap(),
            Response::AdminDatabase(rows)
                if rows.iter().any(|row| row.kind == "Character" && row.id == character_id)
                    && rows.iter().all(|row| !row.detail.contains("password"))
        ));
        let mut stolen = character;
        stolen.id = Some(character_id);
        let error = call(
            &app,
            Request::UpsertCharacter {
                session_token: second_token,
                character: stolen,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("forbidden"));
    }

    #[tokio::test]
    async fn cancellation_registry_stress_is_connection_scoped() {
        let registry: CancellationRegistry = Arc::new(Mutex::new(HashMap::new()));
        let target = Uuid::new_v4();
        let survivor = Uuid::new_v4();
        let mut receivers = Vec::new();
        {
            let mut map = registry.lock().await;
            for request_id in 0..1_000 {
                let (sender, receiver) = watch::channel(false);
                map.insert((target, request_id), sender);
                receivers.push(receiver);
            }
            let (sender, _) = watch::channel(false);
            map.insert((survivor, 1), sender);
        }
        cancel_connection(&registry, target).await;
        assert_eq!(registry.lock().await.len(), 1);
        assert!(registry.lock().await.contains_key(&(survivor, 1)));
        assert!(receivers.iter().all(|receiver| *receiver.borrow()));
    }
}
