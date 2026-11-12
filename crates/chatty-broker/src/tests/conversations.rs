//! Chat titles and group conversation rules.
use super::*;

#[test]
fn generated_chat_titles_are_clean_and_bounded() {
    assert_eq!(
        clean_chat_title("\"A Northern Journey.\"\nignored", "fallback"),
        "A Northern Journey"
    );
    assert_eq!(
        fallback_chat_title("  Plan a journey over the northern road tomorrow please  "),
        "Plan a journey over the northern road"
    );
    assert_eq!(fallback_chat_title("..."), "New chat");
}

#[tokio::test]
async fn group_conversations_require_one_shared_world() {
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
    let (_, registered) = call(
        &app,
        Request::Register {
            username: "group-user".into(),
            password: "group-password".into(),
        },
    )
    .await
    .unwrap();
    let token = match decode::<Response>(&registered).unwrap() {
        Response::Authenticated { session_token, .. } => session_token,
        other => panic!("unexpected response: {other:?}"),
    };

    let mut ids = Vec::new();
    for name in ["Ann", "Bob", "Cara"] {
        let character = CharacterInput {
            id: None,
            name: name.into(),
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
            images: vec![],
            default_image_id: None,
            is_public: false,
            owned_by_user: true,
        };
        let (_, payload) = call(
            &app,
            Request::UpsertCharacter {
                session_token: token.clone(),
                character: Box::new(character),
                world_ids: vec![],
            },
        )
        .await
        .unwrap();
        let Response::Accepted {
            entity_id: Some(id),
            ..
        } = decode::<Response>(&payload).unwrap()
        else {
            panic!("character creation failed")
        };
        ids.push(id);
    }
    let (ann, bob, cara) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    let err = call(
        &app,
        Request::CreateConversation {
            session_token: token.clone(),
            title: "Dup".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann.clone(), ann.clone()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("once"));

    let err = call(
        &app,
        Request::CreateConversation {
            session_token: token.clone(),
            title: "Direct two".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![ann.clone(), bob.clone()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("group reply mode"));

    let err = call(
        &app,
        Request::CreateConversation {
            session_token: token.clone(),
            title: "No world".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann.clone(), bob.clone()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("same world"));

    for (world_id, name, members) in [
        ("w1", "Observatory", vec![ann.clone(), bob.clone()]),
        ("w2", "Harbour", vec![bob.clone(), cara.clone()]),
    ] {
        call(
            &app,
            Request::SaveWorld {
                session_token: token.clone(),
                world: World {
                    id: world_id.into(),
                    name: name.into(),
                    character_ids: members,
                    entries: vec![],
                },
            },
        )
        .await
        .unwrap();
    }

    // Pairwise shared worlds are not enough when no single world links all three.
    let err = call(
        &app,
        Request::CreateConversation {
            session_token: token.clone(),
            title: "Three".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann.clone(), bob.clone(), cara.clone()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("same world"));

    let (_, created) = call(
        &app,
        Request::CreateConversation {
            session_token: token.clone(),
            title: "Duo".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann.clone(), bob.clone()],
        },
    )
    .await
    .unwrap();
    let group_id = match decode::<Response>(&created).unwrap() {
        Response::Accepted {
            entity_id: Some(id),
            ..
        } => id,
        other => panic!("unexpected response: {other:?}"),
    };

    // Adding a character outside the shared world fails.
    let err = call(
        &app,
        Request::UpdateConversation {
            session_token: token.clone(),
            conversation_id: group_id.clone(),
            title: "Duo + Cara".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann.clone(), bob.clone(), cara.clone()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("same world"));

    // Deleting w1 leaves an existing group without a shared world; editing its
    // title or mode without changing members still succeeds.
    call(
        &app,
        Request::DeleteWorld {
            session_token: token.clone(),
            world_id: "w1".into(),
        },
    )
    .await
    .unwrap();
    call(
        &app,
        Request::UpdateConversation {
            session_token: token.clone(),
            conversation_id: group_id.clone(),
            title: "Renamed Duo".into(),
            kind: ConversationKind::GroupRoundRobin,
            participant_ids: vec![bob.clone(), ann.clone()],
        },
    )
    .await
    .unwrap();

    // Linking all three into one world allows adding Cara.
    call(
        &app,
        Request::SaveWorld {
            session_token: token.clone(),
            world: World {
                id: "w2".into(),
                name: "Harbour".into(),
                character_ids: vec![ann.clone(), bob.clone(), cara.clone()],
                entries: vec![],
            },
        },
    )
    .await
    .unwrap();
    call(
        &app,
        Request::UpdateConversation {
            session_token: token.clone(),
            conversation_id: group_id,
            title: "Trio".into(),
            kind: ConversationKind::GroupAutomatic,
            participant_ids: vec![ann, bob, cara],
        },
    )
    .await
    .unwrap();
}
