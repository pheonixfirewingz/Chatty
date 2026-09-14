use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use bytes::Bytes;
use chatty_protocol::util::{
    Context, Error, Result, args::ParsedArgs, bail, format_err, new_uuid, pemfile,
};
use chatty_protocol::*;
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
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore, broadcast, mpsc, watch},
    time::Instant,
};
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

#[path = "systems/admin.rs"]
mod admin;
mod auth;
#[path = "systems/conversations.rs"]
mod conversations;
#[path = "systems/inference.rs"]
mod inference;
mod transport;
mod utils;

use admin::*;
use auth::*;
use conversations::*;
use inference::*;
use transport::*;
use utils::*;

struct Args {
    listen: String,
    database: Option<String>,
    cert: String,
    key: String,
    llama_url: String,
}

/// Usage text printed for `--help` (hand-rolled clap replacement).
const USAGE: &str = "chatty-broker -- TLS 1.3 chat broker\n\n\
Options:\n\
  --listen <addr>     Listen address [env: CHATTY_LISTEN] [default: 0.0.0.0:7443]\n\
  --database <path>   SQLite database URL [env: CHATTY_DATABASE]\n\
  --cert <path>       TLS certificate PEM [default: certs/server.pem]\n\
  --key <path>        TLS private key PEM [default: certs/server.key]\n\
  --llama-url <url>   Inference adapter base URL [env: CHATTY_LLAMA_URL]\n";

#[derive(Clone)]
struct App {
    db: SqlitePool,
    http: reqwest::Client,
    cancellations: CancellationRegistry,
    snapshot_gate: Arc<RwLock<()>>,
    deltas: broadcast::Sender<PublishedDelta>,
    recent_errors: Arc<Mutex<Vec<String>>>,
    argon_gate: Arc<Semaphore>,
}

/// Caps concurrent Argon2 computations so login/register bursts hold a fixed
/// memory ceiling (10 x ~19 MiB) instead of one hash per in-flight attempt.
const ARGON2_CONCURRENCY: usize = 10;

/// Closes connections that go quiet in both directions. Clients hold their
/// session token and Resume on demand; generation streams and admin monitor
/// polling keep their own connection alive through traffic.
const IDLE_CLOSE: Duration = Duration::from_secs(120);

async fn argon_permit(app: &App) -> Result<OwnedSemaphorePermit> {
    app.argon_gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| format_err!("argon gate unavailable: {error}"))
}
type Out = (MessageType, u64, Bytes);
type CancellationRegistry = Arc<Mutex<HashMap<(String, u64), watch::Sender<bool>>>>;
static ACTIVE_CONNECTIONS: AtomicU32 = AtomicU32::new(0);
static STARTED_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static CPU_SAMPLE: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, u64)>>> =
    std::sync::OnceLock::new();

#[derive(Clone)]
struct PublishedDelta {
    owner_id: String,
    origin: String,
    /// Bincode encoding of the delta, computed once at publish time so every
    /// subscriber clones a refcounted buffer instead of re-encoding.
    encoded: Bytes,
}

struct Generation<'a> {
    tx: &'a mpsc::Sender<Out>,
    request_id: u64,
    user_id: &'a str,
    conversation_id: &'a str,
    speaker_id: Option<String>,
    parent_id: Option<String>,
    cancel: watch::Receiver<bool>,
    origin: String,
    /// While >0 the connection must not be idle-closed: a slow model load
    /// can leave the wire silent far past IDLE_CLOSE.
    busy: Arc<AtomicU64>,
}

struct BusyGuard(Arc<AtomicU64>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| format_err!("failed to install TLS crypto provider"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("chatty_broker=info".parse()?),
        )
        .init();
    let parsed = ParsedArgs::parse(USAGE)?;
    let args = Args {
        listen: parsed.string("listen", "CHATTY_LISTEN", "0.0.0.0:7443"),
        database: parsed.optional("database", "CHATTY_DATABASE"),
        cert: parsed.string("cert", "CHATTY_CERT", "certs/server.pem"),
        key: parsed.string("key", "CHATTY_KEY", "certs/server.key"),
        llama_url: parsed.string(
            "llama-url",
            "CHATTY_LLAMA_URL",
            "http://192.168.0.97:11434/v1",
        ),
    };
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
        argon_gate: Arc::new(Semaphore::new(ARGON2_CONCURRENCY)),
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

async fn dispatch(
    app: App,
    tx: mpsc::Sender<Out>,
    identity: Arc<RwLock<Option<String>>>,
    connection_id: String,
    id: u64,
    req: Request,
    busy: Arc<AtomicU64>,
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
            tx.send(($ty, id, encode(&$v)?.into())).await?;
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
            let encoded: Bytes = encode(&delta)?.into();
            tx.send((MessageType::Delta, id, encoded.clone())).await?;
            let _ = app.deltas.send(PublishedDelta {
                owner_id: $owner.to_string(),
                origin: connection_id.clone(),
                encoded,
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
            let uid = new_uuid();
            let salt = SaltString::encode_b64(new_uuid().as_bytes())
                .map_err(|e| Error::msg(e.to_string()))?;
            let hash = {
                let _argon_permit = argon_permit(&app).await?;
                Argon2::default()
                    .hash_password(password.as_bytes(), &salt)
                    .map_err(|e| Error::msg(e.to_string()))?
                    .to_string()
            };
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
                PasswordHash::new(&stored).map_err(|_| format_err!("invalid credentials"))?;
            {
                let _argon_permit = argon_permit(&app).await?;
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .map_err(|_| format_err!("invalid credentials"))?;
            }
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
                "SELECT id,username,role,created_at,prompt_tokens,completion_tokens FROM users ORDER BY created_at LIMIT 1000",
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
                    usage: TokenUsage {
                        prompt_tokens: r.get::<i64, _>("prompt_tokens") as u64,
                        completion_tokens: r.get::<i64, _>("completion_tokens") as u64,
                    },
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
            let uid = new_uuid();
            let salt = SaltString::encode_b64(new_uuid().as_bytes())
                .map_err(|error| Error::msg(error.to_string()))?;
            let hash = {
                let _argon_permit = argon_permit(&app).await?;
                Argon2::default()
                    .hash_password(password.as_bytes(), &salt)
                    .map_err(|error| Error::msg(error.to_string()))?
                    .to_string()
            };
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
        Request::GetAccountUsage { session_token } => {
            let uid = auth(&app.db, &session_token).await?;
            let row = sqlx::query("SELECT prompt_tokens,completion_tokens FROM users WHERE id=?")
                .bind(uid)
                .fetch_one(&app.db)
                .await?;
            send!(
                MessageType::Response,
                Response::AccountUsage(TokenUsage {
                    prompt_tokens: row.get::<i64, _>("prompt_tokens") as u64,
                    completion_tokens: row.get::<i64, _>("completion_tokens") as u64,
                })
            );
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
            config.model = config.model.trim().to_owned();
            config.keep_alive = config.keep_alive.trim().to_owned();
            if !config.adapter_url.starts_with("http://")
                && !config.adapter_url.starts_with("https://")
            {
                bail!("adapter URL must use http or https")
            }
            validate_broker_config(&config)?;
            sqlx::query("UPDATE broker_settings SET adapter_enabled=?,adapter_url=?,use_ollama_api=?,model=?,temperature=?,top_p=?,top_k=?,num_ctx=?,num_predict=?,repeat_penalty=?,seed=?,keep_alive=?,allow_public_characters=?,allow_self_registration=?,updated_at=CURRENT_TIMESTAMP WHERE singleton=1")
                .bind(config.adapter_enabled)
                .bind(&config.adapter_url)
                .bind(config.use_ollama_api)
                .bind(&config.model)
                .bind(config.temperature)
                .bind(config.top_p)
                .bind(config.top_k)
                .bind(config.num_ctx)
                .bind(config.num_predict)
                .bind(config.repeat_penalty)
                .bind(config.seed)
                .bind(&config.keep_alive)
                .bind(config.allow_public_characters)
                .bind(config.allow_self_registration)
                .execute(&app.db)
                .await?;
            send!(MessageType::Response, Response::BrokerConfig(config));
        }
        Request::AdminGetOllamaState { session_token } => {
            require_admin(&app.db, &session_token).await?;
            send!(
                MessageType::Response,
                Response::OllamaState(load_ollama_state(&app).await?)
            );
        }
        Request::AdminOllamaAction {
            session_token,
            action,
        } => {
            require_admin(&app.db, &session_token).await?;
            run_ollama_action(&app, action).await?;
            send!(
                MessageType::Response,
                Response::OllamaState(load_ollama_state(&app).await?)
            );
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
                    description: r.get("description"),
                    personality: r.get("personality"),
                    scenario: r.get("scenario"),
                    system_prompt: r.get("system_prompt"),
                    example_dialogue: r.get("example_dialogue"),
                    appearance: r.get("appearance"),
                    age: r.get("age"),
                    gender: r.get("gender"),
                    race: r.get("race"),
                    misc: r.get("misc"),
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
            match load_owned_conversation(&app.db, &conversation_id, &uid).await? {
                Some(view) => send!(MessageType::Response, Response::ConversationView(view)),
                None => send!(
                    MessageType::Response,
                    Response::ConversationNotFound { conversation_id }
                ),
            }
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
                let encoded: Bytes = encode(&delta)?.into();
                tx.send((MessageType::Delta, id, encoded.clone())).await?;
                let _ = app.deltas.send(PublishedDelta {
                    owner_id: uid.clone(),
                    origin: connection_id.clone(),
                    encoded,
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
        Request::ListWorlds { session_token } => {
            let uid = auth(&app.db, &session_token).await?;
            send!(
                MessageType::Response,
                Response::Worlds(load_worlds(&app.db, &uid).await?)
            );
        }
        Request::ImportSillyTavernWorld {
            session_token,
            source_name,
            lorebook_json,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            let world = import_silly_tavern_world(&app, &uid, &source_name, &lorebook_json).await?;
            send!(MessageType::Response, Response::WorldImportPreview(world));
        }
        Request::SaveWorld {
            session_token,
            mut world,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            if let Err(error) = world.validate() {
                bail!("{error}")
            }
            if world.id.is_empty() {
                world.id = new_uuid();
            }
            let mut transaction = app.db.begin().await?;
            let owner: Option<String> =
                sqlx::query_scalar("SELECT owner_id FROM worlds WHERE id=?")
                    .bind(&world.id)
                    .fetch_optional(&mut *transaction)
                    .await?;
            if owner.as_ref().is_some_and(|owner| owner != &uid) {
                bail!("forbidden world owner")
            }
            for character_id in &world.character_ids {
                let accessible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND (owner_id=? OR is_public=1))")
                    .bind(character_id).bind(&uid).fetch_one(&mut *transaction).await?;
                if !accessible {
                    bail!("character unavailable")
                }
            }
            let used: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(length(data)),0) FROM worlds WHERE owner_id=? AND id<>?",
            )
            .bind(&uid)
            .bind(&world.id)
            .fetch_one(&mut *transaction)
            .await?;
            if used + encode(&world)?.len() as i64 > 4 * 1024 * 1024 {
                bail!("World library exceeds 4 MiB")
            }
            let operation = if owner.is_some() {
                DeltaOperation::Update
            } else {
                DeltaOperation::Add
            };
            let changed = encode(&DeltaPayload::World(world.clone()))?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "world",
                &world.id,
                operation,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO worlds(id,owner_id,data,revision) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,revision=excluded.revision WHERE owner_id=excluded.owner_id")
                .bind(&world.id).bind(&uid).bind(encode(&world)?).bind(rev).execute(&mut *transaction).await?;
            transaction.commit().await?;
            send_delta!(&uid, rev, "world", world.id.clone(), operation, changed);
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(world.id),
                    revision: rev
                }
            );
        }
        Request::DeleteWorld {
            session_token,
            world_id,
        } => {
            let uid = auth(&app.db, &session_token).await?;
            let mut transaction = app.db.begin().await?;
            let deleted = sqlx::query("DELETE FROM worlds WHERE id=? AND owner_id=?")
                .bind(&world_id)
                .bind(&uid)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if deleted == 0 {
                bail!("world not found")
            }
            let changed = encode(&DeltaPayload::Empty)?;
            let rev = delta_tx(
                &mut transaction,
                &uid,
                "world",
                &world_id,
                DeltaOperation::Delete,
                &changed,
            )
            .await?;
            transaction.commit().await?;
            send_delta!(
                &uid,
                rev,
                "world",
                world_id.clone(),
                DeltaOperation::Delete,
                changed
            );
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(world_id),
                    revision: rev
                }
            );
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
            world_ids,
        } => {
            let character = *character;
            let uid = auth(&app.db, &session_token).await?;
            validate_character(&character)?;
            if world_ids.len() > 128 {
                bail!("too many linked worlds")
            }
            let mut requested_world_ids = world_ids;
            requested_world_ids.sort();
            requested_world_ids.dedup();
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
            let eid = character.id.unwrap_or_else(new_uuid);
            let fields = encode(&character.tags)?;
            let mut transaction = app.db.begin().await?;
            let character_revision = delta_tx(
                &mut transaction,
                &uid,
                "character",
                &eid,
                operation,
                &changed,
            )
            .await?;
            sqlx::query("INSERT INTO characters(id,owner_id,name,description,personality,scenario,system_prompt,example_dialogue,appearance,age,gender,race,misc,tags,avatar,revision,is_public) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,personality=excluded.personality,scenario=excluded.scenario,system_prompt=excluded.system_prompt,example_dialogue=excluded.example_dialogue,appearance=excluded.appearance,age=excluded.age,gender=excluded.gender,race=excluded.race,misc=excluded.misc,tags=excluded.tags,avatar=excluded.avatar,revision=excluded.revision,is_public=excluded.is_public WHERE owner_id=excluded.owner_id").bind(&eid).bind(&uid).bind(character.name).bind(character.description).bind(character.personality).bind(character.scenario).bind(character.system_prompt).bind(character.example_dialogue).bind(character.appearance).bind(character.age).bind(character.gender).bind(character.race).bind(character.misc).bind(fields).bind(character.avatar).bind(character_revision).bind(character.is_public).execute(&mut *transaction).await?;

            let world_rows = sqlx::query("SELECT id,data FROM worlds WHERE owner_id=? ORDER BY id")
                .bind(&uid)
                .fetch_all(&mut *transaction)
                .await?;
            if requested_world_ids.iter().any(|requested| {
                !world_rows
                    .iter()
                    .any(|row| row.get::<String, _>("id") == *requested)
            }) {
                bail!("linked world unavailable")
            }
            let mut world_deltas = Vec::new();
            let mut revision = character_revision;
            for row in world_rows {
                let mut world: World = decode(row.get::<&[u8], _>("data"))?;
                let should_link = requested_world_ids.binary_search(&world.id).is_ok();
                let is_linked = world.character_ids.iter().any(|id| id == &eid);
                if should_link == is_linked {
                    continue;
                }
                world.character_ids.retain(|id| id != &eid);
                if should_link {
                    world.character_ids.push(eid.clone());
                }
                world.validate().map_err(Error::msg)?;
                let world_changed = encode(&DeltaPayload::World(world.clone()))?;
                revision = delta_tx(
                    &mut transaction,
                    &uid,
                    "world",
                    &world.id,
                    DeltaOperation::Update,
                    &world_changed,
                )
                .await?;
                sqlx::query("UPDATE worlds SET data=?,revision=? WHERE id=? AND owner_id=?")
                    .bind(encode(&world)?)
                    .bind(revision)
                    .bind(&world.id)
                    .bind(&uid)
                    .execute(&mut *transaction)
                    .await?;
                world_deltas.push((world.id, revision, world_changed));
            }
            transaction.commit().await?;
            send_delta!(
                &uid,
                character_revision,
                "character",
                eid,
                operation,
                changed
            );
            for (world_id, world_revision, world_changed) in world_deltas {
                send_delta!(
                    &uid,
                    world_revision,
                    "world",
                    world_id,
                    DeltaOperation::Update,
                    world_changed
                );
            }
            send!(
                MessageType::Response,
                Response::Accepted {
                    entity_id: Some(eid),
                    revision
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
                    let id = new_uuid();
                    let character = CharacterInput {
                        id: Some(id.clone()),
                        name: "Assistant".into(),
                        description: String::new(),
                        personality: "Helpful, attentive, and conversational.".into(),
                        scenario: String::new(),
                        system_prompt:
                            "Respond naturally and remain consistent with the conversation.".into(),
                        example_dialogue: String::new(),
                        appearance: String::new(),
                        age: String::new(),
                        gender: String::new(),
                        race: String::new(),
                        misc: String::new(),
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
                    sqlx::query("INSERT INTO characters(id,owner_id,name,description,personality,scenario,system_prompt,example_dialogue,appearance,age,gender,race,misc,tags,avatar,revision,is_public) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                        .bind(&id)
                        .bind(&uid)
                        .bind(character.name)
                        .bind(character.description)
                        .bind(character.personality)
                        .bind(character.scenario)
                        .bind(character.system_prompt)
                        .bind(character.example_dialogue)
                        .bind(character.appearance)
                        .bind(character.age)
                        .bind(character.gender)
                        .bind(character.race)
                        .bind(character.misc)
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
            let eid = new_uuid();
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
            let eid = new_uuid();
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
                let encoded: Bytes = encode(&delta)?.into();
                tx.send((MessageType::Delta, id, encoded.clone())).await?;
                let _ = app.deltas.send(PublishedDelta {
                    owner_id: uid,
                    origin: connection_id.clone(),
                    encoded,
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
            let eid = new_uuid();
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
                .insert((connection_id.clone(), id), cancel);
            let result = generate(
                &app,
                Generation {
                    tx: &tx,
                    request_id: id,
                    user_id: &uid,
                    conversation_id: &conversation_id,
                    busy: busy.clone(),
                    speaker_id,
                    parent_id,
                    cancel: cancel_rx,
                    origin: connection_id.clone(),
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
            let eid = memory.id.unwrap_or_else(new_uuid);
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
            let content = extract_memory(&app, &uid, &conversation_id).await?;
            let eid = new_uuid();
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

#[cfg(test)]
mod tests;
