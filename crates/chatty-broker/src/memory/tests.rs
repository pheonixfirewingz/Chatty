use super::*;
use sqlx::sqlite::SqlitePoolOptions;

#[test]
fn fts_queries_drop_stop_words_keep_order_and_match_word_forms() {
    assert_eq!(
        fts_query("Alice met Rowan at the north-gate!"),
        Some("\"alice\"* OR \"met\"* OR \"rowan\"* OR \"north\"* OR \"gate\"*".into())
    );
    assert_eq!(
        fts_query("Cats and stargazing; the CATS were stargazed."),
        Some("\"cat\"* OR \"stargaz\"*".into())
    );
    assert_eq!(fts_query("a I -- the and you"), None);
    let many = (0..40)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>();
    let query = fts_query(&many.join(" ")).unwrap();
    assert_eq!(query.matches(" OR ").count(), 23);
    assert!(query.starts_with("\"word0\"*"));
}

#[test]
fn stop_words_are_sorted_lowercase_and_exclude_negation() {
    assert!(STOP_WORDS.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        STOP_WORDS
            .iter()
            .all(|word| word.chars().all(|c| c.is_ascii_lowercase()))
    );
    for kept in ["not", "no", "never", "tea"] {
        assert!(!is_stop_word(kept));
    }
    assert!(is_stop_word("really"));
}

#[test]
fn retrieval_score_weighs_relevance_importance_recency_and_use() {
    let fresh = retrieval_score(Some(0), 50, 0.0, 0);
    assert!((fresh - (0.5 + 0.15 + 0.2)).abs() < 1e-9);
    // Recency halves every 30 days.
    let month = retrieval_score(None, 0, 30.0, 0);
    assert!((month - 0.1).abs() < 1e-9);
    // Relevance dominates: a matching low-importance memory beats an
    // unrelated important one.
    assert!(retrieval_score(Some(0), 10, 60.0, 0) > retrieval_score(None, 100, 0.0, 0));
    // Use helps a little and is capped.
    assert!(retrieval_score(None, 50, 0.0, 3) > retrieval_score(None, 50, 0.0, 0));
    assert_eq!(
        retrieval_score(None, 50, 0.0, 1_000_000),
        retrieval_score(None, 50, 0.0, 10_000)
    );
}

#[test]
fn prompt_tells_the_model_to_apply_saved_memory_as_continuity() {
    let prompt = memory_prompt(
        "Mara",
        &[MemoryEntry {
            id: "memory".into(),
            conversation_id: None,
            character_id: Some("character".into()),
            kind: MemoryKind::Fact,
            content: "The user appreciates beautiful nighttime views.".into(),
            importance: 50,
            pinned: false,
            confidence: 1.0,
            source: MemorySource::Manual,
            source_message_ids: Vec::new(),
            created_at: "2026-09-18 00:00:00".into(),
            updated_at: "2026-09-18 00:00:00".into(),
            revision: 1,
            status: MemoryStatus::Active,
            superseded_by: None,
        }],
    );
    assert!(prompt.contains("established long-term context"));
    assert!(prompt.contains("even when the user does not repeat them"));
    assert!(prompt.contains("The user appreciates beautiful nighttime views."));
    assert!(!prompt.contains("No applicable saved memories"));
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

async fn memory_db() -> SqlitePool {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    for owner in ["owner", "other"] {
        sqlx::query("INSERT INTO users(id,username,password_hash) VALUES(?,?,'x')")
            .bind(owner)
            .bind(owner)
            .execute(&db)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO characters(id,owner_id,name,description,personality,scenario,system_prompt,example_dialogue,appearance,age,gender,race,misc,tags,avatar,images,default_image_id,revision,is_public) VALUES('character','owner','Mara','','','','','','','','','','',X'',NULL,X'4348494D0100',NULL,1,0)")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Test',0,1)")
        .execute(&db)
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn recency_breaks_ties_and_word_forms_match() {
    let db = memory_db().await;
    for (id, content, updated) in [
        ("old", "The user keeps two cats.", "2025-01-01 00:00:00"),
        (
            "new",
            "The user feeds the cats at dawn.",
            "2026-11-01 00:00:00",
        ),
    ] {
        sqlx::query("INSERT INTO memories(id,owner_id,character_id,content,revision,importance,updated_at) VALUES(?,'owner','character',?,1,50,?)")
            .bind(id)
            .bind(content)
            .bind(updated)
            .execute(&db)
            .await
            .unwrap();
    }
    let memories = relevant_memories(&db, "owner", "chat", "character", "Is the cat hungry?")
        .await
        .unwrap();
    let ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["new", "old"]);
}

#[tokio::test]
async fn retrieval_defers_access_writes_to_one_owner_scoped_flush() {
    let db = memory_db().await;
    sqlx::query("INSERT INTO memories(id,owner_id,character_id,content,revision) VALUES('tea','owner','character','The user loves tea.',1)")
        .execute(&db)
        .await
        .unwrap();
    let access = MemoryAccess::default();
    let counts = || async {
        sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT access_count,last_accessed_at FROM memories WHERE id='tea'",
        )
        .fetch_one(&db)
        .await
        .unwrap()
    };
    for _ in 0..3 {
        let memories = relevant_memories(&db, "owner", "chat", "character", "tea")
            .await
            .unwrap();
        assert_eq!(memories.len(), 1);
        record_memory_access(&access, "owner", &memories);
    }
    // Retrieval itself writes nothing.
    assert_eq!(counts().await, (0, None));
    // Another owner cannot bump this owner's memory.
    let memory = relevant_memories(&db, "owner", "chat", "character", "tea")
        .await
        .unwrap();
    record_memory_access(&access, "other", &memory);
    flush_memory_access(&db, &access).await.unwrap();
    let (count, accessed) = counts().await;
    assert_eq!(count, 3);
    assert!(accessed.is_some());
    assert!(access.lock().unwrap().is_empty());
    // The pending map is bounded.
    let many = (0..MAX_PENDING_ACCESSES + 10)
        .map(|index| MemoryEntry {
            id: format!("m{index}"),
            ..memory[0].clone()
        })
        .collect::<Vec<_>>();
    record_memory_access(&access, "owner", &many);
    assert_eq!(access.lock().unwrap().len(), MAX_PENDING_ACCESSES);
}
