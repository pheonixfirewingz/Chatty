use super::*;
use sha2::{Digest, Sha256};

/// The value stored in `sessions.token`: SHA-256 of the bearer token in hex.
/// A copy of the database therefore holds no usable session tokens.
pub(super) fn session_key(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Replaces plaintext tokens stored by older brokers with their keys, so
/// existing sessions survive the upgrade. Old tokens are two hyphenated
/// UUIDs; keys are hex and never contain a hyphen.
pub(super) async fn hash_legacy_sessions(db: &SqlitePool) -> Result<()> {
    let mut tx = db.begin().await?;
    let legacy: Vec<String> =
        sqlx::query_scalar("SELECT token FROM sessions WHERE instr(token,'-')>0")
            .fetch_all(&mut *tx)
            .await?;
    for token in &legacy {
        sqlx::query("UPDATE sessions SET token=? WHERE token=?")
            .bind(session_key(token))
            .bind(token)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(super) async fn new_session(db: &SqlitePool, uid: &str) -> Result<(String, i64)> {
    let token = chatty_protocol::util::secret_hex(32).context("no secure randomness")?;
    sqlx::query("INSERT INTO sessions(token,user_id) VALUES(?,?)")
        .bind(session_key(&token))
        .bind(uid)
        .execute(db)
        .await?;
    let rev = sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas")
        .fetch_one(db)
        .await?;
    Ok((token, rev))
}
pub(super) async fn auth(db: &SqlitePool, t: &str) -> Result<String> {
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=? AND expires_at>datetime('now')")
        .bind(session_key(t))
        .fetch_optional(db)
        .await?
        .context("unauthorized")
}
pub(super) async fn require_admin(db: &SqlitePool, token: &str) -> Result<String> {
    let row=sqlx::query("SELECT u.id,u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')").bind(session_key(token)).fetch_optional(db).await?.context("unauthorized")?;
    if row.get::<String, _>("role") != "admin" {
        bail!("forbidden")
    }
    Ok(row.get("id"))
}

/// Failed sign-ins allowed per client within [`LOGIN_WINDOW`].
const LOGIN_FAILURES: u32 = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Bounds the throttle's memory. When full, the oldest window is dropped.
const LOGIN_TRACKED: usize = 4096;

/// Throttle key for a sign-in: the client's IPv4 address or IPv6 /64, which
/// one host typically controls whole. Keying by client rather than username
/// stops guessing without letting others lock a user out from elsewhere.
/// Falls back to the username when the address is unknown.
pub(super) fn login_throttle_key(peer: Option<std::net::IpAddr>, username: &str) -> String {
    use std::net::IpAddr;
    match peer.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => {
            let s = ip.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        None => format!("user:{username}"),
    }
}

/// Failed sign-in counts per throttle key, kept in memory only. Failures for
/// unknown usernames count too, so throttling reveals nothing about accounts.
#[derive(Default)]
pub(super) struct LoginThrottle(pub(super) std::sync::Mutex<HashMap<String, (u32, Instant)>>);

impl LoginThrottle {
    pub(super) fn check(&self, username: &str, now: Instant) -> Result<()> {
        let failures = self.0.lock().unwrap();
        match failures.get(username) {
            Some((count, start))
                if *count >= LOGIN_FAILURES && now.duration_since(*start) < LOGIN_WINDOW =>
            {
                bail!("too many failed sign-ins; try again in 15 minutes")
            }
            _ => Ok(()),
        }
    }

    pub(super) fn failed(&self, username: &str, now: Instant) {
        let mut failures = self.0.lock().unwrap();
        if !failures.contains_key(username) && failures.len() >= LOGIN_TRACKED {
            failures.retain(|_, (_, start)| now.duration_since(*start) < LOGIN_WINDOW);
            if failures.len() >= LOGIN_TRACKED
                && let Some(oldest) = failures
                    .iter()
                    .min_by_key(|(_, (_, start))| *start)
                    .map(|(name, _)| name.clone())
            {
                failures.remove(&oldest);
            }
        }
        let entry = failures.entry(username.to_owned()).or_insert((0, now));
        if now.duration_since(entry.1) >= LOGIN_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub(super) fn succeeded(&self, username: &str) {
        self.0.lock().unwrap().remove(username);
    }
}

pub(super) fn login_throttle() -> &'static LoginThrottle {
    static THROTTLE: std::sync::OnceLock<LoginThrottle> = std::sync::OnceLock::new();
    THROTTLE.get_or_init(LoginThrottle::default)
}
