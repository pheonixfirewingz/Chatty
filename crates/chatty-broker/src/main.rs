use adapter_api::{ChatMessage as AdapterMessage, ChatRequest, Completion, GenerationOptions};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use bytes::Bytes;
use chatty_protocol::util::{
    Context, Error, Result, args::ParsedArgs, bail, format_err, new_uuid, pemfile,
};
use chatty_protocol::*;
use futures_util::TryStreamExt;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
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
use tracing::{debug, info, warn};

mod account;
#[path = "../../chatty-adapters/shared/api.rs"]
pub mod adapter_api;
mod adapters;
mod admin;
mod app;
mod characters;
mod conversations;
mod crypto;
mod dispatch;
mod emotion;
mod images;
mod inference;
mod logging;
mod memory;
mod sync;
mod transport;
mod utils;
mod worlds;

use account::*;
use adapters::*;
use admin::*;
use app::*;
use characters::*;
use conversations::*;
use crypto::*;
use dispatch::*;
use emotion::*;

use inference::*;
use logging::*;
use memory::*;
use sync::*;
use transport::*;
use utils::*;
use worlds::*;

struct Args {
    listen: String,
    database: Option<String>,
    log: PathBuf,
    cert: String,
    key: String,
    llama_url: String,
}

/// Usage text printed for `--help` (hand-rolled clap replacement).
const USAGE: &str = "chatty-broker -- TLS 1.3 chat broker\n\n\
Options:\n\
  --listen <addr>     Listen address [env: CHATTY_LISTEN] [default: 0.0.0.0:7443]\n\
  --database <path>   SQLite database URL [env: CHATTY_DATABASE]\n\
  --log <path>        Boot log file [env: CHATTY_LOG]\n\
  --cert <path>       TLS certificate PEM [default: certs/server.pem]\n\
  --key <path>        TLS private key PEM [default: certs/server.key]\n\
  --llama-url <url>   Inference adapter base URL [env: CHATTY_LLAMA_URL]\n\
\nMemory evaluation (no TLS, in-memory database):\n\
  --memory-eval <dir>        Run memory fixtures against --llama-url, then exit\n\
  --memory-eval-model <name> Model for the evaluation [default: first listed]\n\
\nAdapter plugins:\n\
  CHATTY_ADAPTER_DIR       Optional directory of WASM plugin subdirectories\n\
  CHATTY_ADAPTER_PLUGIN    Optional inference/management plugin ID override\n";

#[tokio::main]
async fn main() -> Result<()> {
    let parsed = ParsedArgs::parse(USAGE)?;
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| format_err!("failed to install TLS crypto provider"))?;
    if let Some(directory) = parsed.optional("memory-eval", "") {
        let url = parsed.string("llama-url", "CHATTY_LLAMA_URL", "http://127.0.0.1:18114/v1");
        let model = parsed.optional("memory-eval-model", "");
        let passed = run_memory_evaluation(Path::new(&directory), &url, model.as_deref()).await?;
        std::process::exit(if passed { 0 } else { 1 });
    }
    let log = match parsed.optional("log", "CHATTY_LOG") {
        Some(path) => PathBuf::from(path),
        None => default_broker_log_path()?,
    };
    let args = Args {
        listen: parsed.string("listen", "CHATTY_LISTEN", "0.0.0.0:7443"),
        database: parsed.optional("database", "CHATTY_DATABASE"),
        log,
        cert: parsed.string("cert", "CHATTY_CERT", "certs/server.pem"),
        key: parsed.string("key", "CHATTY_KEY", "certs/server.key"),
        llama_url: parsed.string(
            "llama-url",
            "CHATTY_LLAMA_URL",
            "http://192.168.0.97:11434/v1",
        ),
    };
    init_boot_logging(&args.log)?;
    let _ = STARTED_AT.set(std::time::Instant::now());
    info!(path=%args.log.display(), "boot log initialized");
    let database = match args.database.as_deref() {
        Some(database) => database.to_owned(),
        None => default_database_url()?,
    };
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database)
        .await?;
    sqlx::migrate!().run(&db).await?;
    hash_legacy_sessions(&db).await?;
    sqlx::query("INSERT OR IGNORE INTO broker_settings(singleton,adapter_url) VALUES(1,?)")
        .bind(args.llama_url.trim_end_matches('/'))
        .execute(&db)
        .await?;
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&db)
        .await?;
    let tls = tls_config(&args.cert, &args.key)?;
    let image_key = load_or_create_image_key(&args.key)?;
    let app = App::new(db, AdapterHost::from_environment()?, args.log, image_key);
    app.adapters
        .configuration_changed(&load_broker_config(&app.db).await?)?;
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_net;
