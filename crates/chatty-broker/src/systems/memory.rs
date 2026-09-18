use super::*;

const MAX_MEMORY_CHARS: usize = 4_096;
const RETRIEVED_MEMORY_CHARS: usize = 6_144;

pub(super) fn memory_kind_from_i64(value: i64) -> Result<MemoryKind> {
    match value {
        0 => Ok(MemoryKind::Fact),
        1 => Ok(MemoryKind::Event),
        2 => Ok(MemoryKind::Relationship),
        3 => Ok(MemoryKind::Reflection),
        _ => bail!("invalid stored memory kind"),
    }
}

pub(super) fn memory_source_from_i64(value: i64) -> Result<MemorySource> {
    match value {
        0 => Ok(MemorySource::Manual),
        1 => Ok(MemorySource::Automatic),
        _ => bail!("invalid stored memory source"),
    }
}

fn memory_kind_value(value: MemoryKind) -> i64 {
    match value {
        MemoryKind::Fact => 0,
        MemoryKind::Event => 1,
        MemoryKind::Relationship => 2,
        MemoryKind::Reflection => 3,
    }
}

fn memory_source_value(value: MemorySource) -> i64 {
    match value {
        MemorySource::Manual => 0,
        MemorySource::Automatic => 1,
    }
}

pub(super) fn memory_entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<MemoryEntry> {
    let source_message_ids = serde_json::from_str(row.get::<&str, _>("source_message_ids"))
        .context("invalid memory source message ids")?;
    Ok(MemoryEntry {
        id: row.get("id"),
        conversation_id: row.get("conversation_id"),
        character_id: row.get("character_id"),
        kind: memory_kind_from_i64(row.get("kind"))?,
        content: row.get("content"),
        importance: u8::try_from(row.get::<i64, _>("importance"))
            .context("invalid stored memory importance")?,
        pinned: row.get("pinned"),
        confidence: row.get::<f64, _>("confidence") as f32,
        source: memory_source_from_i64(row.get("source"))?,
        source_message_ids,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        revision: row.get("revision"),
    })
}

pub(super) async fn validate_memory_scope(
    db: &SqlitePool,
    owner_id: &str,
    memory: &mut MemoryInput,
) -> Result<()> {
    memory.content = memory.content.trim().to_owned();
    if memory.content.is_empty() || memory.content.chars().count() > MAX_MEMORY_CHARS {
        bail!("memory must contain between 1 and 4096 characters")
    }
    if memory.importance > 100 {
        bail!("memory importance must be between 0 and 100")
    }
    if let Some(conversation_id) = &memory.conversation_id {
        own_conversation(db, owner_id, conversation_id).await?;
    }
    if let Some(character_id) = &memory.character_id {
        let accessible: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND (owner_id=? OR is_public=1))",
        )
        .bind(character_id)
        .bind(owner_id)
        .fetch_one(db)
        .await?;
        if !accessible {
            bail!("memory character missing or forbidden")
        }
        if let Some(conversation_id) = &memory.conversation_id {
            let participant: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM participants WHERE conversation_id=? AND character_id=?)",
            )
            .bind(conversation_id)
            .bind(character_id)
            .fetch_one(db)
            .await?;
            if !participant {
                bail!("memory character is not a conversation participant")
            }
        }
    }
    Ok(())
}

pub(super) async fn list_memories(
    db: &SqlitePool,
    owner_id: &str,
    conversation_id: Option<&str>,
    character_id: Option<&str>,
) -> Result<Vec<MemoryEntry>> {
    if let Some(conversation_id) = conversation_id {
        own_conversation(db, owner_id, conversation_id).await?;
    }
    let rows = sqlx::query(
        "SELECT id,conversation_id,character_id,kind,content,importance,pinned,confidence,source,source_message_ids,created_at,updated_at,revision \
         FROM memories WHERE owner_id=? AND status=0 \
         AND (? IS NULL OR conversation_id=?) AND (? IS NULL OR character_id=?) \
         ORDER BY pinned DESC,updated_at DESC,revision DESC LIMIT 500",
    )
    .bind(owner_id)
    .bind(conversation_id)
    .bind(conversation_id)
    .bind(character_id)
    .bind(character_id)
    .fetch_all(db)
    .await?;
    rows.iter().map(memory_entry_from_row).collect()
}

pub(super) async fn persist_memory(
    app: &App,
    owner_id: &str,
    mut memory: MemoryInput,
    source: MemorySource,
    confidence: f32,
    source_message_ids: Vec<String>,
) -> Result<(MemoryEntry, DeltaOperation, Vec<u8>)> {
    validate_memory_scope(&app.db, owner_id, &mut memory).await?;
    let id = memory.id.clone().unwrap_or_else(new_uuid);
    let existing = sqlx::query(
        "SELECT owner_id,confidence,source,source_message_ids,created_at FROM memories WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&app.db)
    .await?;
    if existing
        .as_ref()
        .is_some_and(|row| row.get::<String, _>("owner_id") != owner_id)
    {
        bail!("forbidden memory owner")
    }
    let operation = if existing.is_some() {
        DeltaOperation::Update
    } else {
        DeltaOperation::Add
    };
    let now: String = sqlx::query_scalar("SELECT CURRENT_TIMESTAMP")
        .fetch_one(&app.db)
        .await?;
    let (confidence, source, source_message_ids, created_at) = if let Some(row) = existing {
        (
            row.get::<f64, _>("confidence") as f32,
            memory_source_from_i64(row.get("source"))?,
            serde_json::from_str(row.get::<&str, _>("source_message_ids"))
                .context("invalid memory source message ids")?,
            row.get("created_at"),
        )
    } else {
        (
            confidence.clamp(0.0, 1.0),
            source,
            source_message_ids,
            now.clone(),
        )
    };
    let mut entry = MemoryEntry {
        id: id.clone(),
        conversation_id: memory.conversation_id,
        character_id: memory.character_id,
        kind: memory.kind,
        content: memory.content,
        importance: memory.importance,
        pinned: memory.pinned,
        confidence,
        source,
        source_message_ids,
        created_at,
        updated_at: now,
        revision: 0,
    };
    let changed = encode(&DeltaPayload::Memory(entry.clone()))?;
    let mut transaction = app.db.begin().await?;
    let revision = delta_tx(
        &mut transaction,
        owner_id,
        "memory",
        &id,
        operation,
        &changed,
    )
    .await?;
    sqlx::query(
        "INSERT INTO memories(\
            id,owner_id,conversation_id,character_id,content,revision,kind,importance,pinned,\
            confidence,source,source_message_ids,status,created_at,updated_at,last_accessed_at,access_count\
         ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,0,?,?,NULL,0) \
         ON CONFLICT(id) DO UPDATE SET \
            conversation_id=excluded.conversation_id,character_id=excluded.character_id,\
            content=excluded.content,revision=excluded.revision,kind=excluded.kind,\
            importance=excluded.importance,pinned=excluded.pinned,updated_at=excluded.updated_at \
         WHERE owner_id=excluded.owner_id",
    )
    .bind(&entry.id)
    .bind(owner_id)
    .bind(&entry.conversation_id)
    .bind(&entry.character_id)
    .bind(&entry.content)
    .bind(revision)
    .bind(memory_kind_value(entry.kind))
    .bind(i64::from(entry.importance))
    .bind(entry.pinned)
    .bind(f64::from(entry.confidence))
    .bind(memory_source_value(entry.source))
    .bind(serde_json::to_string(&entry.source_message_ids)?)
    .bind(&entry.created_at)
    .bind(&entry.updated_at)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    entry.revision = revision;
    Ok((entry, operation, changed))
}

fn fts_query(value: &str) -> Option<String> {
    let mut words = value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .take(24)
        .collect::<Vec<_>>();
    words.sort_unstable();
    words.dedup();
    (!words.is_empty()).then(|| words.join(" OR "))
}

pub(super) async fn relevant_memories(
    db: &SqlitePool,
    owner_id: &str,
    conversation_id: &str,
    character_id: &str,
    context: &str,
) -> Result<Vec<MemoryEntry>> {
    let baseline = sqlx::query(
        "SELECT id,conversation_id,character_id,kind,content,importance,pinned,confidence,source,source_message_ids,created_at,updated_at,revision \
         FROM memories WHERE owner_id=? AND status=0 \
         AND (conversation_id IS NULL OR conversation_id=?) \
         AND (character_id IS NULL OR character_id=?) \
         ORDER BY pinned DESC,importance DESC,updated_at DESC LIMIT 24",
    )
    .bind(owner_id)
    .bind(conversation_id)
    .bind(character_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(memory_entry_from_row)
    .collect::<Result<Vec<_>>>()?;
    let mut candidates = HashMap::<String, (MemoryEntry, i64)>::new();
    for (rank, entry) in baseline.into_iter().enumerate() {
        let score = if entry.pinned { 100_000 } else { 0 }
            + i64::from(entry.importance) * 100
            + i64::try_from(24usize.saturating_sub(rank)).unwrap_or(0);
        candidates.insert(entry.id.clone(), (entry, score));
    }

    if let Some(query) = fts_query(context) {
        let matches = sqlx::query(
            "SELECT m.id,m.conversation_id,m.character_id,m.kind,m.content,m.importance,m.pinned,m.confidence,m.source,m.source_message_ids,m.created_at,m.updated_at,m.revision \
             FROM memory_fts f JOIN memories m ON m.id=f.memory_id \
             WHERE memory_fts MATCH ? AND m.owner_id=? AND m.status=0 \
             AND (m.conversation_id IS NULL OR m.conversation_id=?) \
             AND (m.character_id IS NULL OR m.character_id=?) \
             ORDER BY bm25(memory_fts),m.importance DESC LIMIT 32",
        )
        .bind(query)
        .bind(owner_id)
        .bind(conversation_id)
        .bind(character_id)
        .fetch_all(db)
        .await?;
        for (rank, entry) in matches.iter().map(memory_entry_from_row).enumerate() {
            let entry = entry?;
            let relevance = 50_000 - i64::try_from(rank).unwrap_or(0) * 100;
            candidates
                .entry(entry.id.clone())
                .and_modify(|candidate| candidate.1 += relevance)
                .or_insert_with(|| {
                    let score = relevance
                        + if entry.pinned { 100_000 } else { 0 }
                        + i64::from(entry.importance) * 100;
                    (entry, score)
                });
        }
    }

    let mut rows = candidates.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
    });
    let mut selected = Vec::new();
    let mut used = 0usize;
    for (memory, _) in rows {
        let length = memory.content.chars().count();
        if selected.len() >= 10 || (used + length > RETRIEVED_MEMORY_CHARS && !memory.pinned) {
            continue;
        }
        used += length;
        selected.push(memory);
    }
    if !selected.is_empty() {
        let ids = selected
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>();
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE memories SET access_count=access_count+1,last_accessed_at=CURRENT_TIMESTAMP WHERE id IN ({placeholders}) AND owner_id=?"
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id);
        }
        query.bind(owner_id).execute(db).await?;
    }
    Ok(selected)
}

pub(super) fn memory_prompt(memories: &[MemoryEntry]) -> String {
    memories
        .iter()
        .map(|memory| {
            let scope = if memory.conversation_id.is_some() {
                "this conversation"
            } else {
                "all conversations"
            };
            format!("- [{}, {}] {}", memory.kind.label(), scope, memory.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn fts_queries_are_bounded_and_ignore_punctuation() {
        assert_eq!(
            fts_query("Alice met Rowan at the north-gate!"),
            Some("\"Alice\" OR \"Rowan\" OR \"gate\" OR \"met\" OR \"north\" OR \"the\"".into())
        );
        assert_eq!(fts_query("a I --"), None);
    }

    #[tokio::test]
    async fn exact_relevance_outranks_unrelated_high_importance_memories() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','owner','x')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO characters(id,owner_id,name,description,personality,scenario,system_prompt,example_dialogue,appearance,age,gender,race,misc,tags,avatar,images,default_image_id,revision,is_public) VALUES('character','owner','Mara','','','','','','','','','','',X'',NULL,X'4348494D0100',NULL,1,0)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Test',0,1)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO memories(id,owner_id,conversation_id,character_id,content,revision,importance) VALUES('relevant','owner',NULL,'character','The user loves stargazing from the old hill.',2,10)")
            .execute(&db)
            .await
            .unwrap();
        for index in 0..12 {
            sqlx::query("INSERT INTO memories(id,owner_id,conversation_id,character_id,content,revision,importance) VALUES(?, 'owner',NULL,'character',?, ?,100)")
                .bind(format!("distractor-{index}"))
                .bind(format!("Unrelated market errand number {index}."))
                .bind(3 + index)
                .execute(&db)
                .await
                .unwrap();
        }

        let memories = relevant_memories(
            &db,
            "owner",
            "chat",
            "character",
            "Would you like to go stargazing tonight?",
        )
        .await
        .unwrap();
        assert_eq!(
            memories.first().map(|memory| memory.id.as_str()),
            Some("relevant")
        );
        assert!(memories.len() <= 10);
    }
}
