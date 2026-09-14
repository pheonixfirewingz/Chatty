use super::*;

pub(super) async fn new_session(db: &SqlitePool, uid: &str) -> Result<(String, i64)> {
    let token = new_uuid() + &new_uuid();
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
pub(super) async fn auth(db: &SqlitePool, t: &str) -> Result<String> {
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=? AND expires_at>datetime('now')")
        .bind(t)
        .fetch_optional(db)
        .await?
        .context("unauthorized")
}
pub(super) async fn require_admin(db: &SqlitePool, token: &str) -> Result<String> {
    let row=sqlx::query("SELECT u.id,u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')").bind(token).fetch_optional(db).await?.context("unauthorized")?;
    if row.get::<String, _>("role") != "admin" {
        bail!("forbidden")
    }
    Ok(row.get("id"))
}
