use super::*;

#[tokio::test]
async fn bounded_timeline_keeps_variant_order_selection_and_owner_scope() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','owner','hash'),('other','other','hash')")
        .execute(&db).await.unwrap();
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Chat',0,1),('other-chat','other','Other',0,2)")
        .execute(&db).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO messages(id,conversation_id,author_type,content,revision) SELECT printf('m%04d',i),'chat','assistant',printf('text %d',i),i+1 FROM n")
        .execute(&db).await.unwrap();
    sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,parent_id,revision) VALUES('child','chat','assistant','branch','m0000',0),('foreign','other-chat','assistant','private',NULL,0)")
        .execute(&db).await.unwrap();
    for (id, message, time) in [
        ("z", "m2000", "2026-01-01"),
        ("b", "m2000", "2026-01-02"),
        ("a", "m2000", "2026-01-02"),
        ("outside", "m0000", "2026-01-01"),
        ("branch", "child", "2026-01-01"),
        ("private", "foreign", "2026-01-01"),
    ] {
        sqlx::query(
            "INSERT INTO variants(id,message_id,content,created_at,revision) VALUES(?,?,?,?,7)",
        )
        .bind(id)
        .bind(message)
        .bind(id)
        .bind(time)
        .execute(&db)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE messages SET selected_variant_id='a' WHERE id='m2000'")
        .execute(&db)
        .await
        .unwrap();

    assert!(
        load_owned_conversation(&db, "chat", "other")
            .await
            .unwrap()
            .is_none()
    );
    let view = load_owned_conversation(&db, "chat", "owner")
        .await
        .unwrap()
        .unwrap();
    // 2001 messages share one created_at second: the limit keeps the
    // newest 2000 and returns them oldest first.
    assert_eq!(view.messages.len(), 2000);
    assert_eq!(view.messages[0].id, "m0001");
    assert_eq!(view.messages[1999].id, "m2000");
    assert!(view.messages.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(
        view.messages[1999].selected_variant_id.as_deref(),
        Some("a")
    );
    assert_eq!(
        view.messages[1999]
            .variants
            .iter()
            .map(|v| v.id.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "b"]
    );
    assert!(view.messages[..1999].iter().all(|m| m.variants.is_empty()));
    sqlx::query("DELETE FROM conversations WHERE id='chat'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('empty','owner','Empty',0,3)")
        .execute(&db).await.unwrap();
    assert!(
        load_conversation(&db, "empty")
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

#[tokio::test]
async fn edited_messages_keep_their_timeline_position() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','owner','hash')")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Chat',0,1)")
        .execute(&db).await.unwrap();
    sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,revision) VALUES('u1','chat','user','hi',2),('c1','chat','character','hello',3),('u2','chat','user','bye',4)")
        .execute(&db).await.unwrap();
    // Editing raises the revision, as UpdateMessage does.
    sqlx::query("UPDATE messages SET content='hello again',revision=9 WHERE id='c1'")
        .execute(&db)
        .await
        .unwrap();
    let view = load_owned_conversation(&db, "chat", "owner")
        .await
        .unwrap()
        .unwrap();
    let order: Vec<&str> = view.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(order, ["u1", "c1", "u2"]);
}

#[tokio::test]
async fn sequence_migration_backfills_timeline_order() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let all = sqlx::migrate!();
    let before = sqlx::migrate::Migrator {
        migrations: all
            .migrations
            .iter()
            .filter(|m| m.version < 19)
            .cloned()
            .collect(),
        ..sqlx::migrate!()
    };
    before.run(&db).await.unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','owner','hash')")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Chat',0,1),('other','owner','Other',0,2)")
        .execute(&db).await.unwrap();
    // Insertion order differs from creation time; same-second rows keep insertion order.
    sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,created_at,revision) VALUES('b','chat','user','b','2026-01-01 10:00:01',3),('a','chat','user','a','2026-01-01 10:00:00',9),('x','other','user','x','2026-01-01 09:00:00',4),('c','chat','user','c','2026-01-01 10:00:01',5)")
        .execute(&db).await.unwrap();
    all.run(&db).await.unwrap();
    sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,revision) VALUES('d','chat','user','d',6)")
        .execute(&db)
        .await
        .unwrap();
    let seq: Vec<(String, i64)> =
        sqlx::query_as("SELECT id,seq FROM messages ORDER BY conversation_id,seq")
            .fetch_all(&db)
            .await
            .unwrap();
    let seq: Vec<(&str, i64)> = seq.iter().map(|(id, n)| (id.as_str(), *n)).collect();
    assert_eq!(seq, [("a", 1), ("b", 2), ("c", 3), ("d", 4), ("x", 1)]);
}
