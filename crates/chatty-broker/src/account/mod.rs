//! Accounts and sessions: registration, sign-in, and per-user queries.
use super::*;

pub(crate) mod handlers;
mod session;
mod throttle;

pub(crate) use session::*;
pub(crate) use throttle::*;

/// A real Argon2 hash with default parameters, verified when a login names an
/// unknown user so that it costs the same as a wrong password.
pub(crate) fn dummy_password_hash() -> Result<&'static str> {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(hash) = HASH.get() {
        return Ok(hash);
    }
    let salt =
        SaltString::encode_b64(b"chatty-dummy-salt").map_err(|e| Error::msg(e.to_string()))?;
    let hash = Argon2::default()
        .hash_password(b"chatty-dummy-password", &salt)
        .map_err(|e| Error::msg(e.to_string()))?
        .to_string();
    Ok(HASH.get_or_init(|| hash))
}

/// Hashes a new password under the shared Argon2 concurrency gate.
pub(crate) async fn hash_password(app: &App, password: &str) -> Result<String> {
    let salt =
        SaltString::encode_b64(new_uuid().as_bytes()).map_err(|e| Error::msg(e.to_string()))?;
    let _permit = app.argon_permit().await?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| Error::msg(e.to_string()))?
        .to_string())
}

pub(crate) fn role_from_db(role: &str) -> Role {
    if role == "admin" {
        Role::Admin
    } else {
        Role::User
    }
}

pub(crate) fn role_to_db(role: Role) -> &'static str {
    if role == Role::Admin { "admin" } else { "user" }
}

pub(crate) async fn user_role(db: &SqlitePool, user_id: &str) -> Result<Role> {
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id=?")
        .bind(user_id)
        .fetch_one(db)
        .await?;
    Ok(role_from_db(&role))
}

pub(crate) fn token_usage(row: &sqlx::sqlite::SqliteRow) -> TokenUsage {
    TokenUsage {
        prompt_tokens: row.get::<i64, _>("prompt_tokens") as u64,
        completion_tokens: row.get::<i64, _>("completion_tokens") as u64,
    }
}

pub(crate) async fn list_users(db: &SqlitePool) -> Result<Vec<UserAccount>> {
    let rows = sqlx::query(
        "SELECT id,username,role,created_at,prompt_tokens,completion_tokens FROM users ORDER BY created_at LIMIT 1000",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|row| UserAccount {
            id: row.get("id"),
            username: row.get("username"),
            role: role_from_db(row.get("role")),
            created_at: row.get("created_at"),
            usage: token_usage(row),
        })
        .collect())
}

pub(crate) fn validate_credentials(u: &str, p: &str) -> Result<()> {
    if u.len() < 3 || u.len() > 64 || p.len() < 10 || p.len() > 1024 {
        bail!("username or password length invalid")
    }
    Ok(())
}
