//! Cross-tenant authorization across every owned entity type.
use super::*;

#[tokio::test]
async fn cross_tenant_character_update_is_forbidden() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,'http://127.0.0.1:1/v1')",
    )
    .execute(&db)
    .await
    .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let (_, first) = call(
        &app,
        Request::Register {
            username: "first-user".into(),
            password: "first-password".into(),
        },
    )
    .await
    .unwrap();
    let first_token = match decode::<Response>(&first).unwrap() {
        Response::Authenticated { session_token, .. } => session_token,
        other => panic!("unexpected response: {other:?}"),
    };
    let (_, default_chat) = call(
        &app,
        Request::CreateConversation {
            session_token: first_token.clone(),
            title: "New chat".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![],
        },
    )
    .await
    .unwrap();
    let default_chat_id = match decode::<Response>(&default_chat).unwrap() {
        Response::Accepted {
            entity_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected response: {other:?}"),
    };
    let default_name: String = sqlx::query_scalar(
            "SELECT c.name FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=?",
        )
        .bind(&default_chat_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(default_name, "Assistant");
    let owner_id: String = sqlx::query_scalar("SELECT user_id FROM sessions WHERE token=?")
        .bind(session_key(&first_token))
        .fetch_one(&app.db)
        .await
        .unwrap();
    let assistant_id: String =
        sqlx::query_scalar("SELECT character_id FROM participants WHERE conversation_id=?")
            .bind(&default_chat_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    persist_generation(
        &app,
        &owner_id,
        &default_chat_id,
        PersistedGeneration {
            speaker_id: &assistant_id,
            generation_id: "original-response",
            content: "The original response",
            parent_id: None,
            character_image_id: Some("default-portrait"),
        },
    )
    .await
    .unwrap();
    for (id, content) in [
        ("variant-one", "First retry"),
        ("variant-two", "Second retry"),
    ] {
        persist_generation(
            &app,
            &owner_id,
            &default_chat_id,
            PersistedGeneration {
                speaker_id: &assistant_id,
                generation_id: id,
                content,
                parent_id: Some("original-response"),
                character_image_id: Some(id),
            },
        )
        .await
        .unwrap();
    }
    let view = load_conversation(&app.db, &default_chat_id).await.unwrap();
    let response = view
        .messages
        .iter()
        .find(|message| message.id == "original-response")
        .unwrap();
    assert_eq!(response.content, "The original response");
    assert_eq!(response.variants.len(), 2);
    assert_eq!(response.selected_variant_id.as_deref(), Some("variant-two"));
    assert_eq!(response.character_image_id.as_deref(), Some("variant-two"));
    assert_eq!(
        view.messages
            .iter()
            .filter(|message| message.parent_id.is_some())
            .count(),
        0
    );
    call(
        &app,
        Request::SelectVariant {
            session_token: first_token.clone(),
            message_id: "original-response".into(),
            variant_id: "original-response".into(),
        },
    )
    .await
    .unwrap();
    let selected: Option<String> =
        sqlx::query_scalar("SELECT selected_variant_id FROM messages WHERE id=?")
            .bind("original-response")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(selected.is_none());
    let (_, sent_message) = call(
        &app,
        Request::SendMessage {
            session_token: first_token.clone(),
            conversation_id: default_chat_id.clone(),
            content: "Plan a journey over the northern road tomorrow please".into(),
            speaker_id: None,
        },
    )
    .await
    .unwrap();
    let sent_message_id = match decode::<Response>(&sent_message).unwrap() {
        Response::Accepted {
            entity_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected response: {other:?}"),
    };
    let default_title: String = sqlx::query_scalar("SELECT title FROM conversations WHERE id=?")
        .bind(&default_chat_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(default_title, "Plan a journey over the northern road");
    call(
        &app,
        Request::UpdateMessage {
            session_token: first_token.clone(),
            message_id: sent_message_id.clone(),
            content: "**Updated** journey plan".into(),
        },
    )
    .await
    .unwrap();
    let updated_content: String = sqlx::query_scalar("SELECT content FROM messages WHERE id=?")
        .bind(&sent_message_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(updated_content, "**Updated** journey plan");
    let update_delta_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deltas WHERE entity_type='message' AND entity_id=? AND operation=1",
    )
    .bind(&sent_message_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(update_delta_count, 1);
    let character = CharacterInput {
        id: None,
        name: "Owner's character".into(),
        description: String::new(),
        personality: String::new(),
        scenario: String::new(),
        system_prompt: String::new(),
        example_dialogue: String::new(),
        appearance: String::new(),
        age: String::new(),
        gender: String::new(),
        race: String::new(),
        misc: String::new(),
        tags: vec![],
        avatar: None,
        images: vec![CharacterImage {
            id: "joy".into(),
            label: "joy".into(),
            data: test_portrait_png(),
        }],
        default_image_id: Some("joy".into()),
        is_public: false,
        owned_by_user: true,
    };
    let (_, created) = call(
        &app,
        Request::UpsertCharacter {
            session_token: first_token.clone(),
            world_ids: vec![],
            character: Box::new(character.clone()),
        },
    )
    .await
    .unwrap();
    let character_id = match decode::<Response>(&created).unwrap() {
        Response::Accepted {
            entity_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected response: {other:?}"),
    };
    let first_delta=sqlx::query("SELECT operation,changed_fields FROM deltas WHERE entity_id=? ORDER BY revision DESC LIMIT 1").bind(&character_id).fetch_one(&app.db).await.unwrap();
    assert_eq!(first_delta.get::<i32, _>("operation"), 0);
    assert!(matches!(
        decode::<DeltaPayload>(first_delta.get::<&[u8], _>("changed_fields")).unwrap(),
        DeltaPayload::Character(character) if character.images.is_empty()
    ));
    let stored_images: Vec<u8> = sqlx::query_scalar("SELECT images FROM characters WHERE id=?")
        .bind(&character_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(stored_images.starts_with(b"CHIMG1"));
    assert!(
        !stored_images
            .windows(character.images[0].data.len())
            .any(|window| window == character.images[0].data)
    );
    let mut updated = character.clone();
    updated.id = Some(character_id.clone());
    updated.is_public = true;
    call(
        &app,
        Request::UpsertCharacter {
            session_token: first_token.clone(),
            world_ids: vec![],
            character: Box::new(updated),
        },
    )
    .await
    .unwrap();
    let update_operation: i32 = sqlx::query_scalar(
        "SELECT operation FROM deltas WHERE entity_id=? ORDER BY revision DESC LIMIT 1",
    )
    .bind(&character_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(update_operation, 1);
    for (kind, should_select) in [
        (ConversationKind::GroupRoundRobin, true),
        (ConversationKind::GroupAutomatic, true),
        (ConversationKind::GroupManual, false),
    ] {
        let (_, created) = call(
            &app,
            Request::CreateConversation {
                session_token: first_token.clone(),
                title: "group".into(),
                kind,
                participant_ids: vec![character_id.clone()],
            },
        )
        .await
        .unwrap();
        let conversation_id = match decode::<Response>(&created).unwrap() {
            Response::Accepted {
                entity_id: Some(id),
                ..
            } => id,
            other => panic!("unexpected response: {other:?}"),
        };
        let participants=sqlx::query("SELECT c.id,c.name,c.system_prompt,c.personality,c.scenario,c.appearance,c.example_dialogue FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? ORDER BY p.position").bind(&conversation_id).fetch_all(&app.db).await.unwrap();
        let selection =
            select_speaker(&app, "test-user", &conversation_id, None, &participants).await;
        if should_select {
            assert_eq!(selection.unwrap(), character_id);
        } else {
            assert!(
                selection
                    .unwrap_err()
                    .to_string()
                    .contains("explicit speaker")
            );
        }
    }
    let (_, created) = call(
        &app,
        Request::CreateConversation {
            session_token: first_token.clone(),
            title: "cascade".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![character_id.clone()],
        },
    )
    .await
    .unwrap();
    let cascade_id = match decode::<Response>(&created).unwrap() {
        Response::Accepted {
            entity_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected response: {other:?}"),
    };
    call(
        &app,
        Request::UpdateConversation {
            session_token: first_token.clone(),
            conversation_id: cascade_id.clone(),
            title: "renamed cascade".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![character_id.clone()],
        },
    )
    .await
    .unwrap();
    let renamed: String = sqlx::query_scalar("SELECT title FROM conversations WHERE id=?")
        .bind(&cascade_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(renamed, "renamed cascade");
    call(
        &app,
        Request::SendMessage {
            session_token: first_token.clone(),
            conversation_id: cascade_id.clone(),
            content: "persisted".into(),
            speaker_id: None,
        },
    )
    .await
    .unwrap();
    call(
        &app,
        Request::UpsertMemory {
            session_token: first_token.clone(),
            memory: MemoryInput {
                id: None,
                conversation_id: Some(cascade_id.clone()),
                character_id: None,
                kind: MemoryKind::Fact,
                content: "memory".into(),
                importance: 50,
                pinned: false,
            },
        },
    )
    .await
    .unwrap();
    call(
        &app,
        Request::DeleteEntity {
            session_token: first_token.clone(),
            kind: EntityKind::Conversation,
            entity_id: cascade_id.clone(),
        },
    )
    .await
    .unwrap();
    for table in ["conversations", "messages"] {
        let sql = format!(
            "SELECT COUNT(*) FROM {table} WHERE {}=?",
            if table == "conversations" {
                "id"
            } else {
                "conversation_id"
            }
        );
        let count: i64 = sqlx::query_scalar(&sql)
            .bind(&cascade_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} was not cascade-deleted");
    }
    let preserved_memory: (i64, Option<String>) = sqlx::query_as(
        "SELECT COUNT(*),MAX(conversation_id) FROM memories WHERE owner_id=(SELECT id FROM users WHERE username='first-user')",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(preserved_memory.0, 1);
    assert_eq!(preserved_memory.1, None);
    let (kind, deleted_conversation) = call(
        &app,
        Request::GetConversation {
            session_token: first_token.clone(),
            conversation_id: cascade_id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(kind, MessageType::Response);
    assert!(matches!(
        decode::<Response>(&deleted_conversation).unwrap(),
        Response::ConversationNotFound { conversation_id }
            if conversation_id == cascade_id
    ));
    let delete_types:Vec<String>=sqlx::query_scalar("SELECT entity_type FROM deltas WHERE operation=2 AND owner_id=(SELECT id FROM users WHERE username='first-user')").fetch_all(&app.db).await.unwrap();
    for expected in ["conversation", "message"] {
        assert!(
            delete_types.iter().any(|kind| kind == expected),
            "missing {expected} delete delta"
        );
    }
    let memory_updates:i64=sqlx::query_scalar("SELECT COUNT(*) FROM deltas WHERE operation=1 AND entity_type='memory' AND owner_id=(SELECT id FROM users WHERE username='first-user')").fetch_one(&app.db).await.unwrap();
    assert_eq!(
        memory_updates, 1,
        "preserved memory should receive an update delta"
    );
    let (snapshot_tx, mut snapshot_rx) = mpsc::channel(64);
    let snapshot_app = app.clone();
    let snapshot_token = first_token.clone();
    let snapshot = tokio::spawn(async move {
        dispatch(
            snapshot_app,
            snapshot_tx,
            Arc::new(RwLock::new(None)),
            new_uuid(),
            77,
            Request::Snapshot {
                session_token: snapshot_token,
            },
            Arc::new(std::sync::atomic::AtomicU64::new(0)).into(),
        )
        .await
    });
    let mut snapshot_entities = HashMap::new();
    while let Some((kind, _, payload)) = snapshot_rx.recv().await {
        match kind {
            MessageType::Delta => {
                let delta: StateDelta = decode(&payload).unwrap();
                snapshot_entities.insert(
                    (delta.entity_type.clone(), delta.entity_id.clone()),
                    decode::<DeltaPayload>(&delta.changed_fields).unwrap(),
                );
            }
            MessageType::Response => {
                assert!(matches!(
                    decode::<Response>(&payload).unwrap(),
                    Response::SyncComplete { .. }
                ));
                break;
            }
            other => panic!("unexpected snapshot frame: {other:?}"),
        }
    }
    snapshot.await.unwrap().unwrap();
    assert!(snapshot_entities.contains_key(&("character".into(), character_id.clone())));
    assert!(!snapshot_entities.contains_key(&("conversation".into(), cascade_id)));
    let (_, second) = call(
        &app,
        Request::Register {
            username: "second-user".into(),
            password: "second-password".into(),
        },
    )
    .await
    .unwrap();
    let second_token = match decode::<Response>(&second).unwrap() {
        Response::Authenticated { session_token, .. } => session_token,
        other => panic!("unexpected response: {other:?}"),
    };
    let forbidden_log_read = call(
        &app,
        Request::AdminReadBrokerLog {
            session_token: second_token.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(forbidden_log_read.to_string().contains("forbidden"));
    let forbidden_search = call(
        &app,
        Request::AdminSearchModels {
            session_token: second_token.clone(),
            query: "llama".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(forbidden_search.to_string().contains("forbidden"));

    let forbidden_message_edit = call(
        &app,
        Request::UpdateMessage {
            session_token: second_token.clone(),
            message_id: sent_message_id.clone(),
            content: "cross-tenant edit".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(forbidden_message_edit.to_string().contains("forbidden"));
    let world = World {
        id: "test-world".into(),
        name: "Moon realm".into(),
        character_ids: vec![character_id.clone()],
        entries: vec![WorldFact {
            title: "Sky".into(),
            content: "Two moons".into(),
            common_knowledge: true,
            enabled: true,
            ..Default::default()
        }],
    };
    call(
        &app,
        Request::SaveWorld {
            session_token: first_token.clone(),
            world: world.clone(),
        },
    )
    .await
    .unwrap();
    let (_, listed) = call(
        &app,
        Request::ListWorlds {
            session_token: first_token.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(decode::<Response>(&listed).unwrap(), Response::Worlds(w) if w.len() == 1 && w[0].entries[0].content == "Two moons")
    );
    let mut character_update = character.clone();
    character_update.id = Some(character_id.clone());
    character_update.is_public = true;
    call(
        &app,
        Request::UpsertCharacter {
            session_token: first_token.clone(),
            character: Box::new(character_update.clone()),
            world_ids: vec![],
        },
    )
    .await
    .unwrap();
    let unlinked: Vec<u8> = sqlx::query_scalar("SELECT data FROM worlds WHERE id=?")
        .bind(&world.id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(
        !decode::<World>(&unlinked)
            .unwrap()
            .character_ids
            .contains(&character_id)
    );
    call(
        &app,
        Request::UpsertCharacter {
            session_token: first_token.clone(),
            character: Box::new(character_update.clone()),
            world_ids: vec![world.id.clone()],
        },
    )
    .await
    .unwrap();
    let linked: Vec<u8> = sqlx::query_scalar("SELECT data FROM worlds WHERE id=?")
        .bind(&world.id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(
        decode::<World>(&linked)
            .unwrap()
            .character_ids
            .contains(&character_id)
    );
    let mut invalid_character_update = character_update;
    invalid_character_update.name = "This must roll back".into();
    assert!(
        call(
            &app,
            Request::UpsertCharacter {
                session_token: first_token.clone(),
                character: Box::new(invalid_character_update),
                world_ids: vec!["missing-world".into()],
            },
        )
        .await
        .is_err()
    );
    let unchanged_name: String = sqlx::query_scalar("SELECT name FROM characters WHERE id=?")
        .bind(&character_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(unchanged_name, "Owner's character");
    let (_, listed) = call(
        &app,
        Request::ListWorlds {
            session_token: second_token.clone(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(decode::<Response>(&listed).unwrap(), Response::Worlds(w) if w.is_empty()));
    assert!(
        call(
            &app,
            Request::SaveWorld {
                session_token: second_token.clone(),
                world: world.clone()
            }
        )
        .await
        .is_err()
    );
    assert!(
        call(
            &app,
            Request::DeleteWorld {
                session_token: second_token.clone(),
                world_id: world.id.clone()
            }
        )
        .await
        .is_err()
    );
    let mut invalid_world = world.clone();
    invalid_world.character_ids = vec!["missing-character".into()];
    assert!(
        call(
            &app,
            Request::SaveWorld {
                session_token: first_token.clone(),
                world: invalid_world
            }
        )
        .await
        .is_err()
    );
    let stored: Vec<u8> = sqlx::query_scalar("SELECT changed_fields FROM deltas WHERE entity_type='world' ORDER BY revision DESC LIMIT 1").fetch_one(&app.db).await.unwrap();
    assert!(
        matches!(decode::<DeltaPayload>(&stored).unwrap(), DeltaPayload::World(w) if w.id == world.id)
    );
    call(
        &app,
        Request::DeleteWorld {
            session_token: first_token.clone(),
            world_id: world.id,
        },
    )
    .await
    .unwrap();

    let (_, listed) = call(
        &app,
        Request::ListCharacters {
            session_token: second_token.clone(),
        },
    )
    .await
    .unwrap();
    let shared = match decode::<Response>(&listed).unwrap() {
        Response::Characters(characters) => characters
            .into_iter()
            .find(|candidate| candidate.id == character_id)
            .expect("public character should be visible"),
        other => panic!("unexpected response: {other:?}"),
    };
    assert!(shared.is_public);
    assert!(!shared.owned_by_user);
    assert_eq!(shared.images.len(), character.images.len());
    assert_eq!(shared.images[0].id, character.images[0].id);
    assert_eq!(shared.images[0].label, character.images[0].label);
    assert!(shared.images[0].data.is_empty());
    assert_eq!(shared.default_image_id.as_deref(), Some("joy"));
    let (_, portrait) = call(
        &app,
        Request::GetCharacterImage {
            session_token: second_token.clone(),
            character_id: character_id.clone(),
            image_id: "joy".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        decode::<Response>(&portrait).unwrap(),
        Response::CharacterImage { character_id: id, image_id, data }
            if id == character_id && image_id == "joy" && data == character.images[0].data
    ));
    call(
        &app,
        Request::CreateConversation {
            session_token: second_token.clone(),
            title: "Shared character chat".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![character_id.clone()],
        },
    )
    .await
    .unwrap();
    call(
        &app,
        Request::AdminSetBrokerConfig {
            session_token: first_token.clone(),
            config: Box::new(BrokerConfig {
                adapter_enabled: false,
                adapter_url: "http://127.0.0.1:11434/v1".into(),
                adapter_plugin: String::new(),
                use_ollama_api: false,
                model: String::new(),
                temperature: 0.8,
                top_p: 0.9,
                top_k: 40,
                num_ctx: 4096,
                num_predict: -1,
                repeat_penalty: 1.1,
                seed: -1,
                keep_alive: "5m".into(),
                allow_public_characters: false,
                allow_self_registration: false,
                image: Default::default(),
                emotion_model: String::new(),
                memory_model: String::new(),
                reasoning: Default::default(),
            }),
        },
    )
    .await
    .unwrap();
    let (_, capabilities) = call(&app, Request::GetServerCapabilities).await.unwrap();
    assert!(matches!(
        decode::<Response>(&capabilities).unwrap(),
        Response::ServerCapabilities {
            registration_enabled: false,
            image_generation: false,
        }
    ));
    call(
        &app,
        Request::AdminCreateUser {
            session_token: first_token.clone(),
            username: "managed-user".into(),
            password: "managed-password".into(),
            role: Role::User,
        },
    )
    .await
    .unwrap();
    let managed_role: String =
        sqlx::query_scalar("SELECT role FROM users WHERE username='managed-user'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(managed_role, "user");
    let self_delete_error = call(
        &app,
        Request::AdminDeleteUser {
            session_token: first_token.clone(),
            user_id: owner_id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(self_delete_error.to_string().contains("own active account"));
    let managed_id: String =
        sqlx::query_scalar("SELECT id FROM users WHERE username='managed-user'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    call(
        &app,
        Request::AdminDeleteUser {
            session_token: first_token.clone(),
            user_id: managed_id,
        },
    )
    .await
    .unwrap();
    let managed_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username='managed-user')")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(!managed_exists);
    let registration_error = call(
        &app,
        Request::Register {
            username: "blocked-user".into(),
            password: "blocked-password".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(registration_error.to_string().contains("registration"));
    let mut blocked_public = character.clone();
    blocked_public.name = "Blocked public character".into();
    blocked_public.is_public = true;
    let publishing_error = call(
        &app,
        Request::UpsertCharacter {
            session_token: second_token.clone(),
            world_ids: vec![],
            character: Box::new(blocked_public),
        },
    )
    .await
    .unwrap_err();
    assert!(publishing_error.to_string().contains("publishing"));
    call(
        &app,
        Request::AdminSetCharacterPublic {
            session_token: first_token.clone(),
            character_id: character_id.clone(),
            is_public: false,
        },
    )
    .await
    .unwrap();
    let is_public: bool = sqlx::query_scalar("SELECT is_public FROM characters WHERE id=?")
        .bind(&character_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!is_public);
    let (_, database) = call(
        &app,
        Request::AdminReadDatabase {
            session_token: first_token,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        decode::<Response>(&database).unwrap(),
        Response::AdminDatabase(rows)
            if rows.iter().any(|row| row.kind == "Character" && row.id == character_id)
                && rows.iter().all(|row| !row.detail.contains("password"))
    ));
    let mut stolen = character;
    stolen.id = Some(character_id);
    let error = call(
        &app,
        Request::UpsertCharacter {
            session_token: second_token,
            world_ids: vec![],
            character: Box::new(stolen),
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("forbidden"));
}
