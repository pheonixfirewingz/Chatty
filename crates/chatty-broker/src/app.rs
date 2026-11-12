//! Shared broker state and the resource bounds that apply to every connection.
use super::*;

/// One memory job per (owner, conversation); the flag requests a follow-up run.
pub(crate) type MemoryJobs = Arc<Mutex<HashMap<(String, String), bool>>>;
/// Outbound frame: message type, request ID, and encoded payload.
pub(crate) type Out = (MessageType, u64, Bytes);
pub(crate) type CancellationRegistry = Arc<Mutex<HashMap<(String, u64), watch::Sender<bool>>>>;

pub(crate) static ACTIVE_CONNECTIONS: AtomicU32 = AtomicU32::new(0);
pub(crate) static STARTED_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
pub(crate) static CPU_SAMPLE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, u64)>>,
> = std::sync::OnceLock::new();

/// Caps concurrent Argon2 computations so login/register bursts hold a fixed
/// memory ceiling (10 x ~19 MiB) instead of one hash per in-flight attempt.
const ARGON2_CONCURRENCY: usize = 10;

/// Caps concurrent character-image blob queries and decryptions so burst fetches
/// cannot stack ~15 MiB per request past the broker's memory ceiling.
const IMAGE_DECRYPT_CONCURRENCY: usize = 2;

/// Live deltas buffered per subscriber before a lagging connection is closed.
const DELTA_BROADCAST_CAPACITY: usize = 256;

/// Closes connections that go quiet in both directions. Clients hold their
/// session token and Resume on demand; generation streams and admin monitor
/// polling keep their own connection alive through traffic.
pub(crate) const IDLE_CLOSE: Duration = Duration::from_secs(180);

#[derive(Clone)]
pub(crate) struct App {
    pub(crate) db: SqlitePool,
    pub(crate) adapters: AdapterHost,
    pub(crate) cancellations: CancellationRegistry,
    pub(crate) snapshot_gate: Arc<RwLock<()>>,
    pub(crate) memory_jobs: MemoryJobs,
    pub(crate) memory_access: MemoryAccess,
    pub(crate) deltas: broadcast::Sender<PublishedDelta>,
    pub(crate) recent_errors: Arc<Mutex<Vec<String>>>,
    pub(crate) log_path: Arc<PathBuf>,
    argon_gate: Arc<Semaphore>,
    image_decrypt_gate: Arc<Semaphore>,
    pub(crate) image_key: Arc<[u8; 32]>,
}

impl App {
    pub(crate) fn new(
        db: SqlitePool,
        adapters: AdapterHost,
        log_path: PathBuf,
        image_key: [u8; 32],
    ) -> Self {
        Self {
            db,
            adapters,
            cancellations: Default::default(),
            snapshot_gate: Default::default(),
            memory_jobs: Default::default(),
            memory_access: Default::default(),
            deltas: broadcast::channel(DELTA_BROADCAST_CAPACITY).0,
            recent_errors: Default::default(),
            log_path: Arc::new(log_path),
            argon_gate: Arc::new(Semaphore::new(ARGON2_CONCURRENCY)),
            image_decrypt_gate: Arc::new(Semaphore::new(IMAGE_DECRYPT_CONCURRENCY)),
            image_key: Arc::new(image_key),
        }
    }

    pub(crate) async fn argon_permit(&self) -> Result<OwnedSemaphorePermit> {
        self.argon_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| format_err!("argon gate unavailable: {error}"))
    }

    pub(crate) async fn image_decrypt_permit(&self) -> Result<OwnedSemaphorePermit> {
        self.image_decrypt_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| format_err!("image decrypt gate unavailable: {error}"))
    }

    /// Fans a persisted delta out to the owner's other connections.
    pub(crate) fn publish(&self, owner_id: &str, origin: &str, encoded: Bytes) {
        let _ = self.deltas.send(PublishedDelta {
            owner_id: owner_id.to_owned(),
            origin: origin.to_owned(),
            encoded,
        });
    }
}

#[derive(Clone)]
pub(crate) struct PublishedDelta {
    pub(crate) owner_id: String,
    pub(crate) origin: String,
    /// Bincode encoding of the delta, computed once at publish time so every
    /// subscriber clones a refcounted buffer instead of re-encoding.
    pub(crate) encoded: Bytes,
}

/// Holds a connection's busy count up while long, possibly silent work runs,
/// so the idle timer does not close it.
pub(crate) struct BusyGuard(Arc<AtomicU64>);

impl BusyGuard {
    pub(crate) fn new(busy: Arc<AtomicU64>) -> Self {
        busy.fetch_add(1, Ordering::Relaxed);
        Self(busy)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
