//! Synchronization requests: full snapshots and delta-log replay.
use super::*;

pub(crate) async fn snapshot(ctx: &Ctx, session_token: &str) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    ctx.set_identity(Some(uid.clone())).await;
    send_snapshot(&ctx.app, ctx.tx(), ctx.id, &uid).await
}

/// Replays the owner's deltas after `since_revision`, then SyncComplete.
/// `resume` re-authenticates a reconnecting client first.
pub(crate) async fn replay(
    ctx: &Ctx,
    session_token: String,
    since_revision: i64,
    resume: bool,
) -> Result<()> {
    let uid = auth(ctx.db(), &session_token).await?;
    ctx.set_identity(Some(uid.clone())).await;
    // A client resuming without state reloads full lists after it
    // authenticates, so replaying the whole delta log would only flash
    // long-deleted entities. Start it at the head like a fresh login.
    let since_revision = if resume && since_revision <= 0 {
        sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas")
            .fetch_one(ctx.db())
            .await?
    } else {
        since_revision
    };
    if resume {
        ctx.respond(Response::Authenticated {
            session_token,
            user_id: uid.clone(),
            role: user_role(ctx.db(), &uid).await?,
            revision: since_revision,
        })
        .await?;
    }
    let mut rows = sqlx::query("SELECT revision,entity_type,entity_id,operation,changed_fields FROM deltas WHERE owner_id=? AND revision>? ORDER BY revision")
        .bind(uid)
        .bind(since_revision)
        .fetch(ctx.db());
    let mut last = since_revision;
    while let Some(row) = rows.try_next().await? {
        last = row.get("revision");
        let delta = StateDelta {
            revision: last,
            entity_type: row.get("entity_type"),
            entity_id: row.get("entity_id"),
            operation: match row.get::<i32, _>("operation") {
                0 => DeltaOperation::Add,
                1 => DeltaOperation::Update,
                _ => DeltaOperation::Delete,
            },
            changed_fields: row.get("changed_fields"),
        };
        ctx.send(MessageType::Delta, &delta).await?;
    }
    ctx.respond(Response::SyncComplete { revision: last }).await
}
