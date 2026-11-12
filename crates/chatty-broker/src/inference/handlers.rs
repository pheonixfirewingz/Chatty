//! Generation requests.
use super::*;

pub(crate) async fn generate(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    speaker_id: Option<String>,
    parent_id: Option<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    if let Some(parent) = &parent_id {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=? AND conversation_id=? AND author_type<>'image')",
        )
        .bind(parent)
        .bind(&conversation_id)
        .fetch_one(ctx.db())
        .await?;
        if !valid {
            bail!("variant parent not found in conversation")
        }
    }
    let (cancel, cancel_rx) = watch::channel(false);
    let key = (ctx.connection_id.clone(), ctx.id);
    ctx.app
        .cancellations
        .lock()
        .await
        .insert(key.clone(), cancel);
    let result = super::generate(
        &ctx.app,
        Generation {
            tx: ctx.tx(),
            request_id: ctx.id,
            user_id: &uid,
            conversation_id: &conversation_id,
            busy: ctx.busy.clone(),
            speaker_id,
            parent_id,
            cancel: cancel_rx,
            origin: ctx.connection_id.clone(),
        },
    )
    .await;
    ctx.app.cancellations.lock().await.remove(&key);
    result
}
