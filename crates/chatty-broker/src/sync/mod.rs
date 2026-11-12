//! Revision log: delta persistence, live fan-out filtering, and snapshots.
use super::*;

pub(crate) mod handlers;

/// Appends one owner-scoped delta inside `transaction`; returns its revision.
pub(crate) async fn delta_tx(
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

pub(crate) fn delta_visible(
    identity: Option<&str>,
    connection_id: &str,
    event: &PublishedDelta,
) -> bool {
    event.origin != connection_id && identity == Some(event.owner_id.as_str())
}

/// Streams every entity visible to `user_id` as Add deltas, then SyncComplete.
pub(crate) async fn send_snapshot(
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
        let payload =
            DeltaPayload::Character(Box::new(character_from_row(app, &row, user_id).await?));
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
    let mut messages=sqlx::query("SELECT m.id,m.conversation_id,m.author_type,m.author_id,m.content,m.parent_id,m.selected_variant_id,m.character_image_id,m.revision FROM messages m JOIN conversations c ON c.id=m.conversation_id WHERE c.owner_id=? AND m.revision<=? ORDER BY m.id").bind(user_id).bind(snapshot_revision).fetch(&app.db);
    while let Some(row) = messages.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Message {
            conversation_id: row.get("conversation_id"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            content: row.get("content"),
            parent_id: row.get("parent_id"),
            selected_variant_id: row.get("selected_variant_id"),
            character_image_id: row.get("character_image_id"),
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
    let memory_sql = format!(
        "SELECT {MEMORY_COLUMNS} FROM memories WHERE owner_id=? AND status=0 AND revision<=? ORDER BY id"
    );
    let mut memories = sqlx::query(&memory_sql)
        .bind(user_id)
        .bind(snapshot_revision)
        .fetch(&app.db);
    while let Some(row) = memories.try_next().await? {
        let entity_id: String = row.get("id");
        let payload = DeltaPayload::Memory(memory_entry_from_row(&row)?);
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
