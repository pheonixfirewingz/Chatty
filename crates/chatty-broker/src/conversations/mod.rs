//! Conversations: ownership checks, loading, deletion, and chat titles.
use super::*;

pub(crate) mod handlers;

pub(crate) async fn own_conversation(db: &SqlitePool, u: &str, c: &str) -> Result<()> {
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=?)",
    )
    .bind(c)
    .bind(u)
    .fetch_one(db)
    .await?
    {
        bail!("conversation not found")
    }
    Ok(())
}

pub(crate) async fn delete_owned_entity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    kind: EntityKind,
    entity_id: &str,
) -> Result<Vec<(&'static str, String)>> {
    let mut deleted = Vec::new();
    match kind {
        EntityKind::Character => {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND owner_id=?)",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let in_use: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM participants WHERE character_id=?)",
            )
            .bind(entity_id)
            .fetch_one(&mut **transaction)
            .await?;
            if in_use {
                bail!("character is used by a conversation; delete that conversation first")
            }
            let memories: Vec<String> =
                sqlx::query_scalar("SELECT id FROM memories WHERE character_id=? AND owner_id=?")
                    .bind(entity_id)
                    .bind(user_id)
                    .fetch_all(&mut **transaction)
                    .await?;
            sqlx::query("DELETE FROM memories WHERE character_id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.extend(memories.into_iter().map(|id| ("memory", id)));
            sqlx::query("DELETE FROM characters WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.push(("character", entity_id.into()));
        }
        EntityKind::Conversation => {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=?)",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let messages: Vec<String> =
                sqlx::query_scalar("SELECT id FROM messages WHERE conversation_id=?")
                    .bind(entity_id)
                    .fetch_all(&mut **transaction)
                    .await?;
            sqlx::query("DELETE FROM conversations WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.extend(messages.into_iter().map(|id| ("message", id)));
            deleted.push(("conversation", entity_id.into()));
        }
        EntityKind::Message => {
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE m.id=? AND c.owner_id=?)").bind(entity_id).bind(user_id).fetch_one(&mut **transaction).await?;
            if !owned {
                bail!("entity not found or forbidden")
            }
            let ids:Vec<String>=sqlx::query_scalar("WITH RECURSIVE branch(id) AS (SELECT ? UNION ALL SELECT m.id FROM messages m JOIN branch b ON m.parent_id=b.id) SELECT id FROM branch").bind(entity_id).fetch_all(&mut **transaction).await?;
            sqlx::query("WITH RECURSIVE branch(id) AS (SELECT ? UNION ALL SELECT m.id FROM messages m JOIN branch b ON m.parent_id=b.id) DELETE FROM messages WHERE id IN(SELECT id FROM branch)").bind(entity_id).execute(&mut **transaction).await?;
            deleted.extend(ids.into_iter().map(|id| ("message", id)));
        }
        EntityKind::Memory => {
            let affected = sqlx::query("DELETE FROM memories WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?
                .rows_affected();
            if affected != 1 {
                bail!("entity not found or forbidden")
            };
            deleted.push(("memory", entity_id.into()));
        }
    }
    Ok(deleted)
}
pub(crate) async fn conversation_from_row(
    db: &SqlitePool,
    row: sqlx::sqlite::SqliteRow,
) -> Result<Conversation> {
    let id: String = row.get("id");
    let participant_ids = sqlx::query_scalar(
        "SELECT character_id FROM participants WHERE conversation_id=? ORDER BY position",
    )
    .bind(&id)
    .fetch_all(db)
    .await?;
    Ok(Conversation {
        id,
        title: row.get("title"),
        kind: ConversationKind::try_from(row.get::<i32, _>("kind"))?,
        participant_ids,
        state: row.get("state"),
        summary: row.get("summary"),
        revision: row.get("revision"),
    })
}

#[cfg(test)]
pub(crate) async fn load_conversation(db: &SqlitePool, id: &str) -> Result<ConversationView> {
    let row = sqlx::query("SELECT id,title,kind,CAST(state AS TEXT) AS state,summary,revision FROM conversations WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await?;
    load_conversation_from_row(db, row).await
}

pub(crate) async fn load_owned_conversation(
    db: &SqlitePool,
    id: &str,
    owner_id: &str,
) -> Result<Option<ConversationView>> {
    let row = sqlx::query("SELECT id,title,kind,CAST(state AS TEXT) AS state,summary,revision FROM conversations WHERE id=? AND owner_id=?")
        .bind(id)
        .bind(owner_id)
        .fetch_optional(db)
        .await?;
    match row {
        Some(row) => Ok(Some(load_conversation_from_row(db, row).await?)),
        None => Ok(None),
    }
}

pub(crate) async fn load_conversation_from_row(
    db: &SqlitePool,
    row: sqlx::sqlite::SqliteRow,
) -> Result<ConversationView> {
    let id: String = row.get("id");
    let conversation = conversation_from_row(db, row).await?;
    // The newest 2000 root messages, oldest first. `seq` is the stable timeline key.
    let rows = sqlx::query("SELECT * FROM (SELECT id,author_type,author_id,content,parent_id,selected_variant_id,character_image_id,created_at,revision,seq FROM messages WHERE conversation_id=? AND parent_id IS NULL ORDER BY seq DESC LIMIT 2000) ORDER BY seq")
        .bind(&id).fetch_all(db).await?;
    // Fetch variants for exactly the bounded timeline, rather than issuing one
    // SQLite round trip per message. Stream rows to avoid a second full row set.
    let message_ids: Vec<&str> = rows.iter().map(|row| row.get("id")).collect();
    let mut variants_by_message = HashMap::<String, Vec<Variant>>::new();
    if !message_ids.is_empty() {
        let mut variants = sqlx::query(
            "SELECT message_id,id,content,created_at,revision FROM variants \
             WHERE message_id IN (SELECT value FROM json_each(?)) \
             ORDER BY message_id,created_at,id",
        )
        .bind(serde_json::to_string(&message_ids)?)
        .fetch(db);
        while let Some(row) = variants.try_next().await? {
            variants_by_message
                .entry(row.get("message_id"))
                .or_default()
                .push(Variant {
                    id: row.get("id"),
                    content: row.get("content"),
                    created_at: row.get("created_at"),
                    revision: row.get("revision"),
                });
        }
    }
    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        let message_id: String = row.get("id");
        let variants = variants_by_message.remove(&message_id).unwrap_or_default();
        messages.push(ChatMessage {
            id: message_id,
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
            character_image_id: row.get("character_image_id"),
            created_at: row.get("created_at"),
            revision: row.get("revision"),
            variants,
        });
    }
    Ok(ConversationView {
        conversation,
        messages,
    })
}

pub(crate) fn fallback_chat_title(message: &str) -> String {
    let title = message
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .trim();
    if title.is_empty() {
        "New chat".into()
    } else {
        clip(title, 80)
    }
}

pub(crate) fn clean_chat_title(candidate: &str, fallback: &str) -> String {
    let title = candidate
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '#' | '*' | '.'))
        .trim();
    if title.is_empty() || title.eq_ignore_ascii_case("new chat") {
        fallback_chat_title(fallback)
    } else {
        clip(title, 80)
    }
}

pub(crate) async fn maybe_name_new_chat(
    app: &App,
    user_id: &str,
    conversation_id: &str,
    first_message: &str,
) -> Result<Option<StateDelta>> {
    let row = sqlx::query(
        "SELECT title,kind,CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=? AND owner_id=?",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(&app.db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    if row.get::<String, _>("title") != "New chat" {
        return Ok(None);
    }
    let user_messages: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE conversation_id=? AND author_type='user' AND parent_id IS NULL",
    )
    .bind(conversation_id)
    .fetch_one(&app.db)
    .await?;
    if user_messages != 1 {
        return Ok(None);
    }

    let fallback = fallback_chat_title(first_message);
    let generated = async {
        let model = selected_model(app).await.ok()?;
        let messages=vec![
            AdapterMessage::new("system","Name this conversation in 2 to 6 words. Return only the title, without quotes or punctuation."),
            AdapterMessage::new("user",first_message),
        ];
        let response=complete(app,&model,messages,GenerationOptions {
            temperature:Some(0.2),max_tokens:Some(24),..Default::default()
        },Duration::from_secs(12)).await.ok()?;
        record_token_usage(app, user_id, adapter_token_usage(response.usage))
            .await
            .ok()?;
        response.content.as_deref()
            .map(|title| clean_chat_title(title, first_message))
    }
    .await;
    let title = generated.unwrap_or(fallback);
    let kind = ConversationKind::try_from(row.get::<i32, _>("kind"))?;
    let participant_ids: Vec<String> = sqlx::query_scalar(
        "SELECT character_id FROM participants WHERE conversation_id=? ORDER BY position",
    )
    .bind(conversation_id)
    .fetch_all(&app.db)
    .await?;
    let changed = encode(&DeltaPayload::Conversation {
        title: title.clone(),
        kind,
        participant_ids,
        state: row.get("state"),
        summary: row.get("summary"),
    })?;
    let mut transaction = app.db.begin().await?;
    let still_unnamed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=? AND owner_id=? AND title='New chat')",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !still_unnamed {
        transaction.rollback().await?;
        return Ok(None);
    }
    let revision = delta_tx(
        &mut transaction,
        user_id,
        "conversation",
        conversation_id,
        DeltaOperation::Update,
        &changed,
    )
    .await?;
    sqlx::query("UPDATE conversations SET title=?,revision=? WHERE id=? AND owner_id=?")
        .bind(title)
        .bind(revision)
        .bind(conversation_id)
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(Some(StateDelta {
        revision,
        entity_type: "conversation".into(),
        entity_id: conversation_id.into(),
        operation: DeltaOperation::Update,
        changed_fields: changed,
    }))
}

/// Rules for a conversation's characters before it is saved: each character
/// appears once, several characters need a group reply mode, and a group's
/// characters must all be linked to one of the owner's worlds. Set
/// `check_world` to false to keep an existing group whose members are
/// unchanged, so groups made before the rule can still be renamed.
pub(crate) async fn check_conversation_characters(
    db: &SqlitePool,
    uid: &str,
    kind: ConversationKind,
    participant_ids: &[String],
    check_world: bool,
) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    if !participant_ids.iter().all(|id| seen.insert(id.as_str())) {
        bail!("a character can only join a conversation once")
    }
    if participant_ids.len() < 2 {
        return Ok(());
    }
    if kind == ConversationKind::Direct {
        bail!("a chat with several characters needs a group reply mode")
    }
    let ids = participant_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if check_world && !chatty_protocol::world::share_world(&load_worlds(db, uid).await?, &ids) {
        bail!("group characters must all belong to the same world; link them to one world first")
    }
    Ok(())
}

#[cfg(test)]
mod tests;
