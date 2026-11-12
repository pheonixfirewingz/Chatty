//! Memory requests.
use super::*;

pub(crate) async fn list(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: Option<String>,
    character_id: Option<String>,
    include_history: bool,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let entries = list_memories(
        ctx.db(),
        &uid,
        conversation_id.as_deref(),
        character_id.as_deref(),
        include_history,
    )
    .await?;
    ctx.respond(Response::Memories(entries)).await
}

pub(crate) async fn upsert(ctx: &Ctx, session_token: &str, memory: MemoryInput) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let (entry, operation, changed) = persist_memory(
        &ctx.app,
        &uid,
        memory,
        MemorySource::Manual,
        1.0,
        Vec::new(),
    )
    .await?;
    ctx.delta(
        &uid,
        entry.revision,
        "memory",
        &entry.id,
        operation,
        changed,
    )
    .await?;
    ctx.accepted(Some(entry.id), entry.revision).await
}

pub(crate) async fn extract(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    character_id: Option<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    if let Some(character_id) = &character_id {
        let participant: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? AND c.id=? AND (c.owner_id=? OR c.is_public=1))")
            .bind(&conversation_id)
            .bind(character_id)
            .bind(&uid)
            .fetch_one(ctx.db())
            .await?;
        if !participant {
            bail!("memory character is not a conversation participant")
        }
    }
    let outcome =
        run_manual_memory_extraction(&ctx.app, &uid, &conversation_id, character_id.as_deref())
            .await?;
    ctx.respond(Response::MemoriesExtracted {
        added: outcome.added,
        updated: outcome.updated,
        retired: outcome.retired,
    })
    .await
}

pub(crate) async fn condense(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: Option<String>,
    character_id: Option<String>,
    kind: Option<MemoryKind>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    if let Some(conversation_id) = &conversation_id {
        own_conversation(ctx.db(), &uid, conversation_id).await?;
    }
    if let Some(character_id) = &character_id
        && !character_accessible(ctx.db(), &uid, character_id).await?
    {
        bail!("memory character missing or forbidden")
    }
    let condensed = condense_memories(
        &ctx.app,
        &uid,
        conversation_id.as_deref(),
        character_id.as_deref(),
        kind,
    )
    .await?;
    for (entity_id, operation, changed, revision) in condensed.deltas {
        ctx.delta(&uid, revision, "memory", &entity_id, operation, changed)
            .await?;
    }
    ctx.respond(Response::MemoriesCondensed {
        before: condensed.before,
        after: condensed.after,
    })
    .await
}
