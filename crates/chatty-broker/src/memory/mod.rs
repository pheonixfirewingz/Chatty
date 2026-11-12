//! Structured character memory: storage and retrieval (ADR 0007), plus
//! model-driven extraction and fixture evaluation.
use super::*;

mod eval;
mod extraction;
pub(crate) mod handlers;

pub(crate) use eval::*;
pub(crate) use extraction::*;

const MAX_MEMORY_CHARS: usize = 4_096;
const RETRIEVED_MEMORY_CHARS: usize = 6_144;
const MEMORY_USAGE_INSTRUCTIONS: &str = concat!(
    "Treat the saved memories below as established long-term context. Apply relevant memories ",
    "naturally in dialogue, decisions, preferences, and continuity, even when the user does not ",
    "repeat them in the current message. Do not contradict them. Do not recite this list, mention ",
    "a memory system, or force an unrelated memory into the response."
);

/// Columns read by `memory_entry_from_row`.
pub(crate) const MEMORY_COLUMNS: &str = "id,conversation_id,character_id,kind,content,importance,pinned,confidence,source,source_message_ids,created_at,updated_at,revision,status,superseded_by";
/// `MEMORY_COLUMNS` qualified for queries that alias `memories` as `m`.
pub(crate) const MEMORY_COLUMNS_M: &str = "m.id,m.conversation_id,m.character_id,m.kind,m.content,m.importance,m.pinned,m.confidence,m.source,m.source_message_ids,m.created_at,m.updated_at,m.revision,m.status,m.superseded_by";
/// Active memories returned by one list request.
const LISTED_ACTIVE_MEMORIES: i64 = 500;
/// Superseded or retired memories returned by one list request.
const LISTED_HISTORY_MEMORIES: i64 = 200;

pub(crate) fn memory_status_from_i64(value: i64) -> Result<MemoryStatus> {
    match value {
        0 => Ok(MemoryStatus::Active),
        1 => Ok(MemoryStatus::Superseded),
        2 => Ok(MemoryStatus::Retired),
        _ => bail!("invalid stored memory status"),
    }
}

pub(crate) fn memory_status_value(value: MemoryStatus) -> i64 {
    match value {
        MemoryStatus::Active => 0,
        MemoryStatus::Superseded => 1,
        MemoryStatus::Retired => 2,
    }
}

pub(crate) fn memory_kind_from_i64(value: i64) -> Result<MemoryKind> {
    match value {
        0 => Ok(MemoryKind::Fact),
        1 => Ok(MemoryKind::Event),
        2 => Ok(MemoryKind::Relationship),
        3 => Ok(MemoryKind::Reflection),
        _ => bail!("invalid stored memory kind"),
    }
}

pub(crate) fn memory_source_from_i64(value: i64) -> Result<MemorySource> {
    match value {
        0 => Ok(MemorySource::Manual),
        1 => Ok(MemorySource::Automatic),
        _ => bail!("invalid stored memory source"),
    }
}

pub(crate) fn memory_kind_value(value: MemoryKind) -> i64 {
    match value {
        MemoryKind::Fact => 0,
        MemoryKind::Event => 1,
        MemoryKind::Relationship => 2,
        MemoryKind::Reflection => 3,
    }
}

pub(crate) fn memory_source_value(value: MemorySource) -> i64 {
    match value {
        MemorySource::Manual => 0,
        MemorySource::Automatic => 1,
    }
}

pub(crate) fn memory_entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<MemoryEntry> {
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
        status: memory_status_from_i64(row.get("status"))?,
        superseded_by: row.get("superseded_by"),
    })
}

pub(crate) async fn validate_memory_scope(
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

pub(crate) async fn list_memories(
    db: &SqlitePool,
    owner_id: &str,
    conversation_id: Option<&str>,
    character_id: Option<&str>,
    include_history: bool,
) -> Result<Vec<MemoryEntry>> {
    if let Some(conversation_id) = conversation_id {
        own_conversation(db, owner_id, conversation_id).await?;
    }
    let mut entries = Vec::new();
    for (status, limit) in [
        ("status=0", LISTED_ACTIVE_MEMORIES),
        ("status<>0", LISTED_HISTORY_MEMORIES),
    ] {
        if status != "status=0" && !include_history {
            break;
        }
        let rows = sqlx::query(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories WHERE owner_id=? AND {status} \
             AND (? IS NULL OR conversation_id IS NULL OR conversation_id=?) AND (? IS NULL OR character_id=?) \
             ORDER BY pinned DESC,updated_at DESC,revision DESC LIMIT ?"
        ))
        .bind(owner_id)
        .bind(conversation_id)
        .bind(conversation_id)
        .bind(character_id)
        .bind(character_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
        for row in &rows {
            entries.push(memory_entry_from_row(row)?);
        }
    }
    Ok(entries)
}

/// Every memory scoped to one owned conversation, history included, so that
/// deleting the conversation can promote all of them to cross-chat scope.
pub(crate) async fn chat_scoped_memories(
    db: &SqlitePool,
    owner_id: &str,
    conversation_id: &str,
) -> Result<Vec<MemoryEntry>> {
    own_conversation(db, owner_id, conversation_id).await?;
    sqlx::query(&format!(
        "SELECT {MEMORY_COLUMNS} FROM memories WHERE owner_id=? AND conversation_id=? ORDER BY revision"
    ))
    .bind(owner_id)
    .bind(conversation_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(memory_entry_from_row)
    .collect()
}

pub(crate) async fn persist_memory(
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
        // Saving a superseded or retired memory makes it active again.
        status: MemoryStatus::Active,
        superseded_by: None,
    };
    let mut transaction = app.db.begin().await?;
    let changed = write_memory_tx(&mut transaction, owner_id, &mut entry, operation).await?;
    transaction.commit().await?;
    Ok((entry, operation, changed))
}

/// Appends the memory delta and writes the row in the caller's transaction.
/// Sets `entry.revision` and returns the encoded delta payload. Callers must
/// have validated scope and ownership; an existing row of another owner is
/// never changed.
pub(crate) async fn write_memory_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    entry: &mut MemoryEntry,
    operation: DeltaOperation,
) -> Result<Vec<u8>> {
    let changed = encode(&DeltaPayload::Memory(entry.clone()))?;
    let revision = delta_tx(
        transaction,
        owner_id,
        "memory",
        &entry.id,
        operation,
        &changed,
    )
    .await?;
    sqlx::query(
        "INSERT INTO memories(\
            id,owner_id,conversation_id,character_id,content,revision,kind,importance,pinned,\
            confidence,source,source_message_ids,status,superseded_by,created_at,updated_at,last_accessed_at,access_count\
         ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,NULL,0) \
         ON CONFLICT(id) DO UPDATE SET \
            conversation_id=excluded.conversation_id,character_id=excluded.character_id,\
            content=excluded.content,revision=excluded.revision,kind=excluded.kind,\
            importance=excluded.importance,pinned=excluded.pinned,updated_at=excluded.updated_at,\
            status=excluded.status,superseded_by=excluded.superseded_by \
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
    .bind(memory_status_value(entry.status))
    .bind(&entry.superseded_by)
    .bind(&entry.created_at)
    .bind(&entry.updated_at)
    .execute(&mut **transaction)
    .await?;
    entry.revision = revision;
    Ok(changed)
}

/// Common English words that carry no topic, sorted for binary search.
/// Retrieval and near-duplicate checks ignore them.
const STOP_WORDS: [&str; 120] = [
    "a", "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at",
    "be", "because", "been", "before", "being", "both", "but", "by", "can", "could", "did", "do",
    "does", "doing", "each", "even", "for", "from", "further", "had", "has", "have", "having",
    "he", "her", "here", "hers", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its",
    "just", "let", "like", "me", "more", "most", "much", "my", "now", "of", "off", "oh", "ok",
    "on", "once", "only", "or", "other", "our", "out", "over", "own", "quite", "really", "said",
    "same", "say", "she", "should", "so", "some", "such", "than", "that", "the", "their", "them",
    "then", "there", "these", "they", "this", "those", "through", "to", "too", "under", "until",
    "up", "us", "very", "was", "way", "we", "well", "were", "what", "when", "where", "which",
    "while", "who", "whom", "why", "will", "with", "would", "yes", "you", "your",
];

pub(crate) fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.binary_search(&word).is_ok()
}

/// Strips a common English suffix so a prefix query also finds other forms:
/// "cats" finds "cat", "stargazing" finds "stargazer". Short words stay whole.
fn search_stem(word: &str) -> &str {
    for (suffix, min_len) in [("ing", 7), ("ed", 6), ("es", 6), ("s", 4)] {
        if word.len() >= min_len
            && !word.ends_with("ss")
            && let Some(stem) = word.strip_suffix(suffix)
        {
            return stem;
        }
    }
    word
}

/// FTS5 query for `value`: distinct non-stop words of three or more
/// characters, in first-seen order (callers put the latest message first),
/// at most 24, ORed as prefix terms. bm25 weights rarer terms higher.
pub(crate) fn fts_query(value: &str) -> Option<String> {
    let mut terms = Vec::<String>::new();
    for word in value.split(|character: char| !character.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() < 3 || is_stop_word(&word) {
            continue;
        }
        let term = format!("\"{}\"*", search_stem(&word).replace('"', "\"\""));
        if !terms.contains(&term) {
            terms.push(term);
            if terms.len() == 24 {
                break;
            }
        }
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Retrieval score weights (Generative Agents: relevance, importance, recency).
const RELEVANCE_WEIGHT: f64 = 0.5;
const IMPORTANCE_WEIGHT: f64 = 0.3;
const RECENCY_WEIGHT: f64 = 0.2;
/// Days for the recency term to halve.
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
/// FTS matches considered per reply; relevance falls linearly over them.
const RELEVANCE_RANKS: usize = 32;
/// Memories placed in one reply prompt.
const RETRIEVED_MEMORIES: usize = 10;

/// Score of an unpinned candidate. `rank` is its bm25 position among FTS
/// matches, if it matched; age counts from its last use or update.
fn retrieval_score(rank: Option<usize>, importance: u8, age_days: f64, access_count: i64) -> f64 {
    let relevance = rank.map_or(0.0, |rank| {
        1.0 - rank.min(RELEVANCE_RANKS) as f64 / RELEVANCE_RANKS as f64
    });
    let recency = 0.5f64.powf(age_days.max(0.0) / RECENCY_HALF_LIFE_DAYS);
    let use_bonus = (0.05 * (1.0 + access_count.max(0) as f64).log2()).min(0.15);
    RELEVANCE_WEIGHT * relevance
        + IMPORTANCE_WEIGHT * f64::from(importance.min(100)) / 100.0
        + RECENCY_WEIGHT * recency
        + use_bonus
}

/// Selects at most ten active memories for the speaking character within a
/// fixed character budget: pinned memories first, then by `retrieval_score`.
/// Reading is free of writes; callers record use with `record_memory_access`.
pub(crate) async fn relevant_memories(
    db: &SqlitePool,
    owner_id: &str,
    conversation_id: &str,
    character_id: &str,
    context: &str,
) -> Result<Vec<MemoryEntry>> {
    const AGE: &str = "julianday('now')-julianday(COALESCE(max(last_accessed_at,updated_at),updated_at)) AS age_days,access_count";
    const AGE_M: &str = "julianday('now')-julianday(COALESCE(max(m.last_accessed_at,m.updated_at),m.updated_at)) AS age_days,m.access_count";
    let mut candidates = HashMap::<String, (MemoryEntry, Option<usize>, f64, i64)>::new();
    let baseline = sqlx::query(&format!(
        "SELECT {MEMORY_COLUMNS},{AGE} FROM memories WHERE owner_id=? AND status=0 \
         AND (conversation_id IS NULL OR conversation_id=?) \
         AND (character_id IS NULL OR character_id=?) \
         ORDER BY pinned DESC,importance DESC,updated_at DESC LIMIT 24",
    ))
    .bind(owner_id)
    .bind(conversation_id)
    .bind(character_id)
    .fetch_all(db)
    .await?;
    for row in &baseline {
        let entry = memory_entry_from_row(row)?;
        let age = row.get::<Option<f64>, _>("age_days").unwrap_or_default();
        candidates.insert(
            entry.id.clone(),
            (entry, None, age, row.get("access_count")),
        );
    }
    if let Some(query) = fts_query(context) {
        let matches = sqlx::query(&format!(
            "SELECT {MEMORY_COLUMNS_M},{AGE_M} FROM memory_fts f JOIN memories m ON m.id=f.memory_id \
             WHERE memory_fts MATCH ? AND m.owner_id=? AND m.status=0 \
             AND (m.conversation_id IS NULL OR m.conversation_id=?) \
             AND (m.character_id IS NULL OR m.character_id=?) \
             ORDER BY bm25(memory_fts),m.importance DESC LIMIT {RELEVANCE_RANKS}",
        ))
        .bind(query)
        .bind(owner_id)
        .bind(conversation_id)
        .bind(character_id)
        .fetch_all(db)
        .await?;
        for (rank, row) in matches.iter().enumerate() {
            let entry = memory_entry_from_row(row)?;
            let age = row.get::<Option<f64>, _>("age_days").unwrap_or_default();
            candidates
                .entry(entry.id.clone())
                .and_modify(|candidate| candidate.1 = Some(rank))
                .or_insert_with(|| (entry, Some(rank), age, row.get("access_count")));
        }
    }

    let mut rows = candidates
        .into_values()
        .map(|(entry, rank, age, uses)| {
            let score = retrieval_score(rank, entry.importance, age, uses);
            (entry, score)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .0
            .pinned
            .cmp(&left.0.pinned)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
    });
    let mut selected = Vec::new();
    let mut used = 0usize;
    for (memory, _) in rows {
        let length = memory.content.chars().count();
        if selected.len() >= RETRIEVED_MEMORIES
            || (used + length > RETRIEVED_MEMORY_CHARS && !memory.pinned)
        {
            continue;
        }
        used += length;
        selected.push(memory);
    }
    Ok(selected)
}

/// Retrieval counts waiting to be written, per (owner, memory).
pub(crate) type MemoryAccess = Arc<std::sync::Mutex<HashMap<(String, String), i64>>>;
/// Memories with pending counts. Further new ones are dropped until a flush;
/// counts only influence ranking.
const MAX_PENDING_ACCESSES: usize = 4096;

/// Counts one use of each memory in memory. A reply costs no database write.
pub(crate) fn record_memory_access(
    access: &MemoryAccess,
    owner_id: &str,
    memories: &[MemoryEntry],
) {
    let Ok(mut pending) = access.lock() else {
        return;
    };
    for memory in memories {
        let key = (owner_id.to_owned(), memory.id.clone());
        if let Some(count) = pending.get_mut(&key) {
            *count = count.saturating_add(1);
        } else if pending.len() < MAX_PENDING_ACCESSES {
            pending.insert(key, 1);
        }
    }
}

/// Writes pending retrieval counts in one transaction. The memory job calls
/// it after each run; counts still pending at a crash are lost.
pub(crate) async fn flush_memory_access(db: &SqlitePool, access: &MemoryAccess) -> Result<()> {
    let pending = match access.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return Ok(()),
    };
    if pending.is_empty() {
        return Ok(());
    }
    let mut transaction = db.begin().await?;
    for ((owner_id, id), count) in pending {
        sqlx::query(
            "UPDATE memories SET access_count=access_count+?,last_accessed_at=CURRENT_TIMESTAMP WHERE id=? AND owner_id=?",
        )
        .bind(count)
        .bind(id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Reply-prompt section for the speaking character's memories.
pub(crate) fn memory_prompt(holder: &str, memories: &[MemoryEntry]) -> String {
    if memories.is_empty() {
        return "No applicable saved memories.".into();
    }
    let entries = memories
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
        .join("\n");
    format!(
        "{MEMORY_USAGE_INSTRUCTIONS}\nMemories held by {}, written in the third person; \"the user\" is the person you are talking with:\n{entries}",
        clip(holder, 128)
    )
}

#[cfg(test)]
mod tests;
