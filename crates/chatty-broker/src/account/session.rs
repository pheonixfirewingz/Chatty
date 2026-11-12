//! Session tokens and authorization checks.
use super::*;
use sha2::{Digest, Sha256};

/// The value stored in `sessions.token`: SHA-256 of the bearer token in hex.
/// A copy of the database therefore holds no usable session tokens.
pub(crate) fn session_key(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Replaces plaintext tokens stored by older brokers with their keys, so
/// existing sessions survive the upgrade. Old tokens are two hyphenated
/// UUIDs; keys are hex and never contain a hyphen.
pub(crate) async fn hash_legacy_sessions(db: &SqlitePool) -> Result<()> {
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

pub(crate) async fn new_session(db: &SqlitePool, uid: &str) -> Result<(String, i64)> {
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
pub(crate) async fn auth(db: &SqlitePool, t: &str) -> Result<String> {
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=? AND expires_at>datetime('now')")
        .bind(session_key(t))
        .fetch_optional(db)
        .await?
        .context("unauthorized")
}
pub(crate) async fn require_admin(db: &SqlitePool, token: &str) -> Result<String> {
    let row=sqlx::query("SELECT u.id,u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')").bind(session_key(token)).fetch_optional(db).await?.context("unauthorized")?;
    if row.get::<String, _>("role") != "admin" {
        bail!("forbidden")
    }
    Ok(row.get("id"))
}
