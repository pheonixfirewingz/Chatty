use super::*;

pub(super) async fn own_conversation(db: &SqlitePool, u: &str, c: &str) -> Result<()> {
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

pub(super) async fn delete_owned_entity(
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
            let memories: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM memories WHERE conversation_id=? AND owner_id=?",
            )
            .bind(entity_id)
            .bind(user_id)
            .fetch_all(&mut **transaction)
            .await?;
            sqlx::query("DELETE FROM memories WHERE conversation_id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("DELETE FROM conversations WHERE id=? AND owner_id=?")
                .bind(entity_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            deleted.extend(messages.into_iter().map(|id| ("message", id)));
            deleted.extend(memories.into_iter().map(|id| ("memory", id)));
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
pub(super) async fn delta_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    entity_type: &str,
    entity_id: &str,
    operation: DeltaOperation,
    changed_fields: &[u8],
) -> Result<i64> {
    let result = sqlx::query("INSERT INTO deltas(owner_id,entity_type,entity_id,operation,changed_fields) VALUES(?,?,?,?,?)")
        .bind(user_id).bind(entity_type).bind(entity_id)
        .bind(match operation { DeltaOperation::Add => 0, DeltaOperation::Update => 1, DeltaOperation::Delete => 2 })
        .bind(changed_fields).execute(&mut **transaction).await?;
    Ok(result.last_insert_rowid())
}

pub(super) async fn conversation_from_row(
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
pub(super) async fn load_conversation(db: &SqlitePool, id: &str) -> Result<ConversationView> {
    let row = sqlx::query("SELECT id,title,kind,CAST(state AS TEXT) AS state,summary,revision FROM conversations WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await?;
    load_conversation_from_row(db, row).await
}

pub(super) async fn load_owned_conversation(
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

pub(super) async fn load_conversation_from_row(
    db: &SqlitePool,
    row: sqlx::sqlite::SqliteRow,
) -> Result<ConversationView> {
    let id: String = row.get("id");
    let conversation = conversation_from_row(db, row).await?;
    let rows = sqlx::query("SELECT id,author_type,author_id,content,parent_id,selected_variant_id,created_at,revision FROM messages WHERE conversation_id=? AND parent_id IS NULL ORDER BY revision,id LIMIT 2000")
        .bind(&id).fetch_all(db).await?;
    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        let message_id: String = row.get("id");
        let variant_rows = sqlx::query(
            "SELECT id,content,created_at,revision FROM variants WHERE message_id=? ORDER BY created_at,id",
        )
        .bind(&message_id)
        .fetch_all(db)
        .await?;
        let variants = variant_rows
            .into_iter()
            .map(|v| Variant {
                id: v.get("id"),
                content: v.get("content"),
                created_at: v.get("created_at"),
                revision: v.get("revision"),
            })
            .collect();
        messages.push(ChatMessage {
            id: message_id,
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
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

pub(super) async fn send_snapshot(
    app: &App,
    tx: &mpsc::Sender<Out>,
    request_id: u64,
    user_id: &str,
) -> Result<()> {
    let snapshot_revision: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas WHERE owner_id=?")
            .bind(user_id)
            .fetch_one(&app.db)
            .await?;
    let mut characters = sqlx::query(
        "SELECT * FROM characters WHERE (owner_id=? AND revision<=?) OR is_public=1 ORDER BY id",
    )
    .bind(user_id)
    .bind(snapshot_revision)
    .fetch(&app.db);
    while let Some(row) = characters.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Character(CharacterInput {
            id: Some(entity_id.clone()),
            name: row.get("name"),
            description: row.get("description"),
            personality: row.get("personality"),
            scenario: row.get("scenario"),
            system_prompt: row.get("system_prompt"),
            example_dialogue: row.get("example_dialogue"),
            appearance: row.get("appearance"),
            age: row.get("age"),
            gender: row.get("gender"),
            race: row.get("race"),
            misc: row.get("misc"),
            tags: decode(row.get::<&[u8], _>("tags")).unwrap_or_default(),
            avatar: row.get("avatar"),
            is_public: row.get("is_public"),
            owned_by_user: row.get::<String, _>("owner_id") == user_id,
        });
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "character".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?.into()))
            .await?;
    }
    drop(characters);
    let mut conversations=sqlx::query("SELECT c.id,c.title,c.kind,CAST(c.state AS TEXT) AS state,c.summary,c.revision,(SELECT json_group_array(character_id) FROM (SELECT character_id FROM participants WHERE conversation_id=c.id ORDER BY position)) AS participant_ids FROM conversations c WHERE c.owner_id=? AND c.revision<=? ORDER BY c.id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = conversations.try_next().await? {
        let entity_id: String = row.get("id");
        let participant_ids: Vec<String> =
            serde_json::from_str(row.get::<&str, _>("participant_ids"))?;
        let payload = DeltaPayload::Conversation {
            title: row.get("title"),
            kind: ConversationKind::try_from(row.get::<i32, _>("kind"))?,
            participant_ids,
            state: row.get("state"),
            summary: row.get("summary"),
        };
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "conversation".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?.into()))
            .await?;
    }
    drop(conversations);
    let mut messages=sqlx::query("SELECT m.id,m.conversation_id,m.author_type,m.author_id,m.content,m.parent_id,m.selected_variant_id,m.revision FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE c.owner_id=? AND m.revision<=? ORDER BY m.id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = messages.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Message {
            conversation_id: row.get("conversation_id"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
        };
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "message".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?.into()))
            .await?;
    }
    drop(messages);
    let worlds = sqlx::query(
        "SELECT id,data,revision FROM worlds WHERE owner_id=? AND revision<=? ORDER BY id",
    )
    .bind(user_id)
    .bind(snapshot_revision)
    .fetch_all(&app.db)
    .await?;
    for row in worlds {
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "world".into(),
            entity_id: row.get("id"),
            operation: DeltaOperation::Add,
            changed_fields: encode(&DeltaPayload::World(decode(row.get::<&[u8], _>("data"))?))?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?.into()))
            .await?;
    }
    let mut memories=sqlx::query("SELECT id,conversation_id,character_id,content,revision FROM memories WHERE owner_id=? AND revision<=? ORDER BY id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = memories.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Memory(MemoryInput {
            id: Some(entity_id.clone()),
            conversation_id: row.get("conversation_id"),
            character_id: row.get("character_id"),
            content: row.get("content"),
        });
        let delta = StateDelta {
            revision: row.get("revision"),
            entity_type: "memory".into(),
            entity_id,
            operation: DeltaOperation::Add,
            changed_fields: encode(&payload)?,
        };
        tx.send((MessageType::Delta, request_id, encode(&delta)?.into()))
            .await?;
    }
    tx.send((
        MessageType::Response,
        request_id,
        encode(&Response::SyncComplete {
            revision: snapshot_revision,
        })?
        .into(),
    ))
    .await?;
    Ok(())
}

pub(super) fn fallback_chat_title(message: &str) -> String {
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

pub(super) fn clean_chat_title(candidate: &str, fallback: &str) -> String {
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

pub(super) async fn maybe_name_new_chat(
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
        let response = app
            .http
            .post(format!("{}/chat/completions", adapter_url(app).await.ok()?))
            .timeout(Duration::from_secs(12))
            .json(&json!({
                "model": &model,
                "messages": [
                    {"role":"system","content":"Name this conversation in 2 to 6 words. Return only the title, without quotes or punctuation."},
                    {"role":"user","content": first_message}
                ],
                "stream": false,
                "temperature": 0.2,
                "max_tokens": 24
            }))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        record_token_usage(app, user_id, token_usage_from_openai(&response))
            .await
            .ok()?;
        response["choices"][0]["message"]["content"]
            .as_str()
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

pub(super) async fn load_worlds(db: &SqlitePool, uid: &str) -> Result<Vec<World>> {
    let rows: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT data FROM worlds WHERE owner_id=? ORDER BY id")
            .bind(uid)
            .fetch_all(db)
            .await?;
    rows.iter()
        .map(|data| decode(data).map_err(Into::into))
        .collect()
}
