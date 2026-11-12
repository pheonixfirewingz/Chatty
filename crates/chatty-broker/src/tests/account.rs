//! Sessions, sign-in, and the login throttle.
use super::*;

#[tokio::test]
async fn saved_session_resume_does_not_depend_on_adapter() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_enabled,adapter_url) VALUES(1,0,'http://127.0.0.1:1/v1')")
            .execute(&db)
            .await
            .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let (_, registered) = call(
        &app,
        Request::Register {
            username: "resume-user".into(),
            password: "resume-password".into(),
        },
    )
    .await
    .unwrap();
    let token = match decode::<Response>(&registered).unwrap() {
        Response::Authenticated { session_token, .. } => session_token,
        other => panic!("unexpected response: {other:?}"),
    };
    let user_id: String = sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=?")
        .bind(session_key(&token))
        .fetch_one(&app.db)
        .await
        .unwrap();
    record_token_usage(
        &app,
        &user_id,
        TokenUsage {
            prompt_tokens: 120,
            completion_tokens: 30,
        },
    )
    .await
    .unwrap();
    let (_, usage_response) = call(
        &app,
        Request::GetAccountUsage {
            session_token: token.clone(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        decode::<Response>(&usage_response).unwrap(),
        Response::AccountUsage(TokenUsage {
            prompt_tokens: 120,
            completion_tokens: 30,
        })
    ));
    let (_, users_response) = call(
        &app,
        Request::AdminListUsers {
            session_token: token.clone(),
        },
    )
    .await
    .unwrap();
    let Response::Users(users) = decode::<Response>(&users_response).unwrap() else {
        panic!("expected users response")
    };
    assert_eq!(users[0].usage.total(), 150);
    let (kind, missing) = call(
        &app,
        Request::GetConversation {
            session_token: token.clone(),
            conversation_id: "already-deleted".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(kind, MessageType::Response);
    assert!(matches!(
        decode::<Response>(&missing).unwrap(),
        Response::ConversationNotFound { conversation_id }
            if conversation_id == "already-deleted"
    ));

    // A deleted conversation's history must not reach a stateless resume.
    for operation in [0, 2] {
        sqlx::query(
            "INSERT INTO deltas(owner_id,entity_type,entity_id,operation,changed_fields) VALUES(?,'conversation','gone',?,x'')",
        )
        .bind(&user_id)
        .bind(operation)
        .execute(&app.db)
        .await
        .unwrap();
    }
    let (tx, mut rx) = mpsc::channel(32);
    let resume_app = app.clone();
    let resume = tokio::spawn(async move {
        dispatch(
            resume_app,
            tx,
            Arc::new(RwLock::new(None)),
            new_uuid(),
            2,
            Request::Resume {
                session_token: token,
                since_revision: 0,
            },
            Arc::new(std::sync::atomic::AtomicU64::new(0)).into(),
        )
        .await
    });
    let mut authenticated = false;
    let mut synchronized = false;
    let mut replayed = 0;
    while let Some((kind, _, payload)) = rx.recv().await {
        if kind == MessageType::Delta {
            replayed += 1;
        }
        if kind != MessageType::Response {
            continue;
        }
        match decode::<Response>(&payload).unwrap() {
            Response::Authenticated { role, .. } => {
                authenticated = true;
                assert_eq!(role, Role::Admin);
            }
            Response::SyncComplete { .. } => {
                synchronized = true;
                break;
            }
            _ => {}
        }
    }
    resume.await.unwrap().unwrap();
    assert!(authenticated);
    assert!(synchronized);
    // A stateless resume starts at the head instead of replaying old deltas.
    assert_eq!(replayed, 0);
    let monitor = broker_monitor(&app).await;
    assert_eq!(monitor.adapter_status, AdapterStatus::Disabled);
    assert_eq!(monitor.adapter_model_count, 0);
}

#[test]
fn unknown_user_login_verifies_a_real_dummy_hash() {
    let parsed = PasswordHash::new(dummy_password_hash().unwrap()).unwrap();
    assert_eq!(parsed.algorithm, argon2::Algorithm::default().ident());
    assert!(
        Argon2::default()
            .verify_password(b"any password", &parsed)
            .is_err()
    );
}

#[tokio::test]
async fn sessions_store_only_token_hashes_and_legacy_tokens_keep_working() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO users(id,username,password_hash,role) VALUES('u','u','unused','user')",
    )
    .execute(&db)
    .await
    .unwrap();
    let legacy = format!("{}{}", new_uuid(), new_uuid());
    sqlx::query("INSERT INTO sessions(token,user_id) VALUES(?,'u')")
        .bind(&legacy)
        .execute(&db)
        .await
        .unwrap();
    hash_legacy_sessions(&db).await.unwrap();
    hash_legacy_sessions(&db).await.unwrap();
    assert_eq!(auth(&db, &legacy).await.unwrap(), "u");

    let (token, _) = new_session(&db, "u").await.unwrap();
    assert_eq!(auth(&db, &token).await.unwrap(), "u");
    let stored: Vec<String> = sqlx::query_scalar("SELECT token FROM sessions")
        .fetch_all(&db)
        .await
        .unwrap();
    assert!(!stored.contains(&token) && !stored.contains(&legacy));
    assert!(auth(&db, &session_key(&token)).await.is_err());
}

#[test]
fn login_throttle_blocks_after_repeated_failures_until_the_window_ends() {
    let throttle = LoginThrottle::default();
    let start = Instant::now();
    for _ in 0..10 {
        throttle.check("198.51.100.7", start).unwrap();
        throttle.failed("198.51.100.7", start);
    }
    assert!(throttle.check("198.51.100.7", start).is_err());
    assert!(throttle.check("198.51.100.8", start).is_ok());
    let later = start + Duration::from_secs(15 * 60);
    throttle.check("198.51.100.7", later).unwrap();
    throttle.failed("198.51.100.7", later);
    assert!(throttle.check("198.51.100.7", later).is_ok());

    for _ in 0..10 {
        throttle.failed("203.0.113.1", start);
    }
    throttle.succeeded("203.0.113.1");
    assert!(throttle.check("203.0.113.1", start).is_ok());
}

#[test]
fn login_throttle_keys_by_client_address() {
    let v4: std::net::IpAddr = "198.51.100.7".parse().unwrap();
    let mapped: std::net::IpAddr = "::ffff:198.51.100.7".parse().unwrap();
    let v6a: std::net::IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
    let v6b: std::net::IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
    assert_eq!(login_throttle_key(Some(v4), "alice"), "198.51.100.7");
    assert_eq!(login_throttle_key(Some(mapped), "bob"), "198.51.100.7");
    assert_eq!(
        login_throttle_key(Some(v6a), "a"),
        login_throttle_key(Some(v6b), "b")
    );
    assert_eq!(login_throttle_key(Some(v6a), "a"), "2001:db8:1:2::/64");
    assert_eq!(login_throttle_key(None, "alice"), "user:alice");
}

#[test]
fn login_throttle_memory_is_bounded() {
    let throttle = LoginThrottle::default();
    let start = Instant::now();
    for index in 0..5000 {
        throttle.failed(
            &format!("user{index}"),
            start + Duration::from_millis(index),
        );
    }
    assert_eq!(throttle.0.lock().unwrap().len(), 4096);
}
