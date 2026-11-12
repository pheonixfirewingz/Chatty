//! Small helpers shared across broker subsystems.
use super::*;

pub(crate) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) async fn record_token_usage(app: &App, user_id: &str, usage: TokenUsage) -> Result<()> {
    if usage.total() == 0 {
        return Ok(());
    }
    sqlx::query("UPDATE users SET prompt_tokens=prompt_tokens+?,completion_tokens=completion_tokens+? WHERE id=?")
        .bind(i64::try_from(usage.prompt_tokens).context("prompt token count overflow")?)
        .bind(i64::try_from(usage.completion_tokens).context("completion token count overflow")?)
        .bind(user_id)
        .execute(&app.db)
        .await?;
    Ok(())
}
