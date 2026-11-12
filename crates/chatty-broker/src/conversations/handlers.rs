//! Conversation and message requests.
use super::*;

const MAX_MESSAGE_BYTES: usize = 65_536;
const MAX_CONTEXT_BYTES: usize = 262_144;
const MAX_TITLE_BYTES: usize = 512;
const MAX_PARTICIPANTS: usize = 32;

fn validate_message(content: &str) -> Result<()> {
    if content.is_empty() || content.len() > MAX_MESSAGE_BYTES {
        bail!("message length invalid")
    }
    Ok(())
}

fn validate_header(title: &str, participant_ids: &[String]) -> Result<()> {
    if participant_ids.is_empty()
        || participant_ids.len() > MAX_PARTICIPANTS
        || title.is_empty()
        || title.len() > MAX_TITLE_BYTES
    {
        bail!("conversation title or participant count invalid")
    }
    Ok(())
}

/// Every participant must exist and be owned by the caller or public.
async fn check_participants_accessible(
    db: &SqlitePool,
    uid: &str,
    participant_ids: &[String],
) -> Result<()> {
    let accessible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM characters WHERE (owner_id=? OR is_public=1) AND id IN (SELECT value FROM json_each(?))")
        .bind(uid)
        .bind(serde_json::to_string(participant_ids)?)
        .fetch_one(db)
        .await?;
    if accessible != participant_ids.len() as i64 {
        bail!("one or more participants are missing or forbidden")
    }
    Ok(())
}

pub(crate) async fn list(ctx: &Ctx, session_token: &str) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    // Participants come back as one JSON array per row, so listing needs a
    // single query instead of one participant query per conversation.
    let rows = sqlx::query("SELECT c.id,c.title,c.kind,CAST(c.state AS TEXT) AS state,c.summary,c.revision,(SELECT json_group_array(character_id) FROM (SELECT character_id FROM participants WHERE conversation_id=c.id ORDER BY position)) AS participant_ids FROM conversations c WHERE c.owner_id=? ORDER BY c.revision DESC LIMIT 500")
        .bind(&uid)
        .fetch_all(ctx.db())
        .await?;
    let conversations = rows
        .iter()
        .map(|row| {
            Ok(Conversation {
                id: row.get("id"),
                title: row.get("title"),
                kind: ConversationKind::try_from(row.get::<i32, _>("kind"))?,
                participant_ids: serde_json::from_str(row.get::<&str, _>("participant_ids"))?,
                state: row.get("state"),
                summary: row.get("summary"),
                revision: row.get("revision"),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ctx.respond(Response::Conversations(conversations)).await
}

pub(crate) async fn get(ctx: &Ctx, session_token: &str, conversation_id: String) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    ctx.respond(
        match load_owned_conversation(ctx.db(), &conversation_id, &uid).await? {
            Some(view) => Response::ConversationView(view),
            None => Response::ConversationNotFound { conversation_id },
        },
    )
    .await
}

/// The caller's first character, or a new default assistant when they have none.
async fn default_participant(ctx: &Ctx, uid: &str) -> Result<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM characters WHERE owner_id=? ORDER BY revision,id LIMIT 1",
    )
    .bind(uid)
    .fetch_optional(ctx.db())
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = new_uuid();
    let character = default_assistant(id.clone());
    let changed = encode(&DeltaPayload::Character(Box::new(character.clone())))?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        uid,
        "character",
        &id,
        DeltaOperation::Add,
        &changed,
    )
    .await?;
    write_character(&ctx.app, &mut transaction, uid, &id, character, rev).await?;
    transaction.commit().await?;
    ctx.delta(uid, rev, "character", &id, DeltaOperation::Add, changed)
        .await?;
    Ok(id)
}

pub(crate) async fn create(
    ctx: &Ctx,
    session_token: &str,
    title: String,
    kind: ConversationKind,
    mut participant_ids: Vec<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    if participant_ids.is_empty() {
        if !matches!(kind, ConversationKind::Direct) {
            bail!("group conversation requires participants")
        }
        participant_ids.push(default_participant(ctx, &uid).await?);
    }
    validate_header(&title, &participant_ids)?;
    check_conversation_characters(ctx.db(), &uid, kind, &participant_ids, true).await?;
    check_participants_accessible(ctx.db(), &uid, &participant_ids).await?;
    let eid = new_uuid();
    let changed = encode(&DeltaPayload::Conversation {
        title: title.clone(),
        kind,
        participant_ids: participant_ids.clone(),
        state: String::new(),
        summary: String::new(),
    })?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "conversation",
        &eid,
        DeltaOperation::Add,
        &changed,
    )
    .await?;
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES(?,?,?,?,?)")
        .bind(&eid)
        .bind(&uid)
        .bind(title)
        .bind(kind as i32)
        .bind(rev)
        .execute(&mut *transaction)
        .await?;
    for (position, character_id) in participant_ids.iter().enumerate() {
        sqlx::query("INSERT INTO participants(conversation_id,character_id,position) SELECT ?,id,? FROM characters WHERE id=? AND (owner_id=? OR is_public=1)")
            .bind(&eid)
            .bind(position as i32)
            .bind(character_id)
            .bind(&uid)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "conversation",
        &eid,
        DeltaOperation::Add,
        changed,
    )
    .await?;
    ctx.accepted(Some(eid), rev).await
}

pub(crate) async fn update(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    title: String,
    kind: ConversationKind,
    participant_ids: Vec<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    validate_header(&title, &participant_ids)?;
    let mut current_ids: Vec<String> =
        sqlx::query_scalar("SELECT character_id FROM participants WHERE conversation_id=?")
            .bind(&conversation_id)
            .fetch_all(ctx.db())
            .await?;
    let mut requested_ids = participant_ids.clone();
    current_ids.sort();
    requested_ids.sort();
    check_conversation_characters(
        ctx.db(),
        &uid,
        kind,
        &participant_ids,
        current_ids != requested_ids,
    )
    .await?;
    check_participants_accessible(ctx.db(), &uid, &participant_ids).await?;
    let (state, summary): (String, String) =
        sqlx::query_as("SELECT CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=?")
            .bind(&conversation_id)
            .fetch_one(ctx.db())
            .await?;
    let changed = encode(&DeltaPayload::Conversation {
        title: title.clone(),
        kind,
        participant_ids: participant_ids.clone(),
        state,
        summary,
    })?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "conversation",
        &conversation_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE conversations SET title=?,kind=?,revision=? WHERE id=?")
        .bind(title)
        .bind(kind as i32)
        .bind(rev)
        .bind(&conversation_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM participants WHERE conversation_id=?")
        .bind(&conversation_id)
        .execute(&mut *transaction)
        .await?;
    for (position, character_id) in participant_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO participants(conversation_id,character_id,position) VALUES(?,?,?)",
        )
        .bind(&conversation_id)
        .bind(character_id)
        .bind(position as i32)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "conversation",
        &conversation_id,
        DeltaOperation::Update,
        changed,
    )
    .await?;
    ctx.accepted(Some(conversation_id), rev).await
}

pub(crate) async fn update_state(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    state: String,
    summary: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    if state.len() > MAX_CONTEXT_BYTES || summary.len() > MAX_CONTEXT_BYTES {
        bail!("conversation state or summary too large")
    }
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    let changed = encode(&DeltaPayload::ConversationContext {
        state: state.clone(),
        summary: summary.clone(),
    })?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "conversation",
        &conversation_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE conversations SET state=?,summary=?,revision=? WHERE id=?")
        .bind(state)
        .bind(summary)
        .bind(rev)
        .bind(&conversation_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "conversation",
        &conversation_id,
        DeltaOperation::Update,
        changed,
    )
    .await?;
    ctx.accepted(Some(conversation_id), rev).await
}

pub(crate) async fn delete_entity(
    ctx: &Ctx,
    session_token: &str,
    kind: EntityKind,
    entity_id: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    // Deleting a chat keeps its chat-scoped memories as character memories.
    let mut promoted_memories = if matches!(kind, EntityKind::Conversation) {
        chat_scoped_memories(ctx.db(), &uid, &entity_id).await?
    } else {
        Vec::new()
    };
    let now: String = sqlx::query_scalar("SELECT CURRENT_TIMESTAMP")
        .fetch_one(ctx.db())
        .await?;
    let mut transaction = ctx.db().begin().await?;
    let mut rev = 0;
    let mut outgoing = Vec::new();
    for memory in &mut promoted_memories {
        memory.conversation_id = None;
        memory.updated_at.clone_from(&now);
        let changed = encode(&DeltaPayload::Memory(memory.clone()))?;
        rev = delta_tx(
            &mut transaction,
            &uid,
            "memory",
            &memory.id,
            DeltaOperation::Update,
            &changed,
        )
        .await?;
        sqlx::query("UPDATE memories SET conversation_id=NULL,updated_at=?,revision=? WHERE id=? AND owner_id=?")
            .bind(&now)
            .bind(rev)
            .bind(&memory.id)
            .bind(&uid)
            .execute(&mut *transaction)
            .await?;
        outgoing.push(StateDelta {
            revision: rev,
            entity_type: "memory".into(),
            entity_id: memory.id.clone(),
            operation: DeltaOperation::Update,
            changed_fields: changed,
        });
    }
    let deletions = delete_owned_entity(&mut transaction, &uid, kind, &entity_id).await?;
    let empty = encode(&DeltaPayload::Empty)?;
    for (entity_type, id) in deletions {
        rev = delta_tx(
            &mut transaction,
            &uid,
            entity_type,
            &id,
            DeltaOperation::Delete,
            &empty,
        )
        .await?;
        outgoing.push(StateDelta {
            revision: rev,
            entity_type: entity_type.into(),
            entity_id: id,
            operation: DeltaOperation::Delete,
            changed_fields: empty.clone(),
        });
    }
    transaction.commit().await?;
    for delta in &outgoing {
        ctx.deliver(&uid, delta).await?;
    }
    ctx.accepted(Some(entity_id), rev).await
}

/// Inserts a root message and delivers its delta. Returns its ID and revision.
async fn add_message(
    ctx: &Ctx,
    uid: &str,
    conversation_id: &str,
    author_type: &str,
    author_id: Option<String>,
    content: &str,
) -> Result<(String, i64)> {
    let eid = new_uuid();
    let changed = encode(&DeltaPayload::Message {
        conversation_id: conversation_id.into(),
        author_type: author_type.into(),
        author_id: author_id.clone(),
        content: content.into(),
        parent_id: None,
        selected_variant_id: None,
        character_image_id: None,
    })?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        uid,
        "message",
        &eid,
        DeltaOperation::Add,
        &changed,
    )
    .await?;
    sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES(?,?,?,?,?,?)")
        .bind(&eid)
        .bind(conversation_id)
        .bind(author_type)
        .bind(author_id)
        .bind(content)
        .bind(rev)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    ctx.delta(uid, rev, "message", &eid, DeltaOperation::Add, changed)
        .await?;
    Ok((eid, rev))
}

pub(crate) async fn send_message(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    content: String,
    speaker_id: Option<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    validate_message(&content)?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    if let Some(speaker) = &speaker_id {
        let participant: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM participants WHERE conversation_id=? AND character_id=?)",
        )
        .bind(&conversation_id)
        .bind(speaker)
        .fetch_one(ctx.db())
        .await?;
        if !participant {
            bail!("message speaker is not a conversation participant")
        }
    }
    let author_type = if speaker_id.is_some() {
        "character"
    } else {
        "user"
    };
    let user_message = speaker_id.is_none();
    let (eid, rev) = add_message(
        ctx,
        &uid,
        &conversation_id,
        author_type,
        speaker_id,
        &content,
    )
    .await?;
    ctx.accepted(Some(eid), rev).await?;
    // The first user message names a new chat after the response, so the
    // client is not kept waiting on the title model.
    if user_message
        && let Some(delta) = maybe_name_new_chat(&ctx.app, &uid, &conversation_id, &content).await?
    {
        ctx.deliver(&uid, &delta).await?;
    }
    Ok(())
}

pub(crate) async fn update_message(
    ctx: &Ctx,
    session_token: &str,
    message_id: String,
    content: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    validate_message(&content)?;
    let mut transaction = ctx.db().begin().await?;
    let row = sqlx::query(
        "SELECT m.conversation_id,m.author_type,m.author_id,m.parent_id,m.selected_variant_id,m.character_image_id \
         FROM messages m JOIN conversations c ON c.id=m.conversation_id \
         WHERE m.id=? AND c.owner_id=?",
    )
    .bind(&message_id)
    .bind(&uid)
    .fetch_optional(&mut *transaction)
    .await?
    .context("message not found or forbidden")?;
    let changed = encode(&DeltaPayload::Message {
        conversation_id: row.get("conversation_id"),
        author_type: row.get("author_type"),
        author_id: row.get("author_id"),
        content: content.clone(),
        parent_id: row.get("parent_id"),
        selected_variant_id: row.get("selected_variant_id"),
        character_image_id: row.get("character_image_id"),
    })?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "message",
        &message_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE messages SET content=?,revision=? WHERE id=?")
        .bind(content)
        .bind(rev)
        .bind(&message_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "message",
        &message_id,
        DeltaOperation::Update,
        changed,
    )
    .await?;
    ctx.accepted(Some(message_id), rev).await
}

pub(crate) async fn send_system_message(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    content: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    validate_message(&content)?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    let (eid, rev) = add_message(ctx, &uid, &conversation_id, "system", None, &content).await?;
    ctx.accepted(Some(eid), rev).await
}

pub(crate) async fn select_variant(
    ctx: &Ctx,
    session_token: &str,
    message_id: String,
    variant_id: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE m.id=? AND c.owner_id=? AND (?=m.id OR EXISTS(SELECT 1 FROM variants v WHERE v.message_id=m.id AND v.id=?)))")
        .bind(&message_id)
        .bind(&uid)
        .bind(&variant_id)
        .bind(&variant_id)
        .fetch_one(ctx.db())
        .await?;
    if !valid {
        bail!("message or variant not found")
    }
    let selected = (variant_id != message_id).then_some(variant_id);
    let changed = encode(&DeltaPayload::VariantSelection {
        variant_id: selected.clone(),
    })?;
    let mut transaction = ctx.db().begin().await?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "message",
        &message_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE messages SET selected_variant_id=?,revision=? WHERE id=?")
        .bind(&selected)
        .bind(rev)
        .bind(&message_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "message",
        &message_id,
        DeltaOperation::Update,
        changed,
    )
    .await?;
    ctx.accepted(Some(message_id), rev).await
}
