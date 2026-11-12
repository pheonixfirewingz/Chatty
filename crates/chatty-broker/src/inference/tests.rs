use super::*;

mod prompt_budget_tests {
    use super::*;

    #[test]
    fn long_chats_keep_three_verbatim_and_twenty_compacted_turns() {
        let history = (0..30)
            .rev()
            .map(|index| {
                let role = if index % 2 == 0 { "user" } else { "assistant" };
                (
                    role.to_owned(),
                    format!("turn-{index}\n{}", "spaced   history ".repeat(40)),
                )
            })
            .collect::<Vec<_>>();
        let expected_recent = history[..3].to_vec();
        let messages =
            budget_chat_messages("short system", "Long-term memory:\nnone", history, 20_000)
                .unwrap();

        assert_eq!(messages.len(), 24);
        for (offset, (role, content)) in expected_recent.iter().rev().enumerate() {
            let message = &messages[21 + offset];
            assert_eq!(message.role, role.as_str());
            assert_eq!(message.content, content.as_str());
        }
        assert!(
            messages[1]
                .content
                .as_str()
                .starts_with("[Earlier turn, compacted] turn-7 spaced history")
        );
        assert!(
            messages[20]
                .content
                .as_str()
                .starts_with("[Earlier turn, compacted] turn-26 spaced history")
        );
        assert!(
            !messages
                .iter()
                .any(|message| { message.content.contains("turn-6") })
        );
    }

    #[test]
    fn compacted_history_stays_inside_the_admin_context_budget() {
        let history = (0..80)
            .rev()
            .map(|index| {
                let role = if index % 2 == 0 { "user" } else { "assistant" };
                (role.to_owned(), format!("turn-{index} {}", "x".repeat(600)))
            })
            .collect::<Vec<_>>();
        let messages = budget_chat_messages(
            &"system ".repeat(2_000),
            "Long-term memory:\nnone",
            history,
            3_072,
        )
        .unwrap();
        let total = messages
            .iter()
            .map(|message| estimated_message_tokens(message.content.as_str()))
            .sum::<usize>();

        assert!(total <= 3_072);
        assert!(
            messages
                .iter()
                .any(|message| { message.content.contains("turn-79") })
        );
        assert_eq!(
            messages.last().unwrap().content,
            format!("turn-79 {}", "x".repeat(600))
        );
        assert!(
            !messages
                .iter()
                .any(|message| { message.content.contains("turn-0") })
        );
    }

    #[test]
    fn refuses_to_silently_truncate_the_three_verbatim_turns() {
        let history = (0..3)
            .map(|index| {
                (
                    "user".to_owned(),
                    format!("turn-{index} {}", "x".repeat(900)),
                )
            })
            .collect::<Vec<_>>();
        let error =
            budget_chat_messages("system", "Long-term memory:\nnone", history, 128).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("admin-configured context window")
        );
    }

    #[test]
    fn clipping_preserves_selected_long_term_memory() {
        let system = format!("Character instructions.\n{}", "background ".repeat(4_000));
        let priority = format!(
            "Long-term memory:\n- [fact, all conversations] {}\n{}",
            "The user always orders jasmine tea.",
            "less relevant memory. ".repeat(500)
        );
        let messages = budget_chat_messages(&system, &priority, Vec::new(), 1_024).unwrap();
        let sent_system = messages[0].content.as_str();

        assert!(sent_system.contains("Character instructions."));
        assert!(sent_system.contains("The user always orders jasmine tea."));
        assert!(estimated_message_tokens(sent_system) <= 512);
    }

    #[tokio::test]
    async fn prompt_history_skips_image_messages() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('u','u','x')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','u','Chat',0,1)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,revision) VALUES('a','chat','user','hello',1),('b','chat','image','a long image prompt',2),('c','chat','character','hi',3)")
            .execute(&db)
            .await
            .unwrap();
        let rows = sqlx::query(PROMPT_HISTORY)
            .bind("chat")
            .fetch_all(&db)
            .await
            .unwrap();
        let history: Vec<_> = rows.iter().map(|row| history_message(row, None)).collect();
        assert_eq!(
            history,
            [
                ("assistant".to_owned(), "hi".to_owned()),
                ("user".to_owned(), "hello".to_owned())
            ]
        );
    }

    #[test]
    fn prompt_budget_reserves_output_space() {
        let mut config = BrokerConfig {
            adapter_enabled: true,
            adapter_url: String::new(),
            adapter_plugin: String::new(),
            use_ollama_api: true,
            model: String::new(),
            temperature: 0.8,
            top_p: 0.9,
            top_k: 40,
            num_ctx: 4_096,
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
        };
        assert_eq!(prompt_input_budget(&config), 3_072);
        config.num_predict = 512;
        assert_eq!(prompt_input_budget(&config), 3_584);
        config.num_predict = 8_192;
        assert_eq!(prompt_input_budget(&config), 2_048);
    }
}

pub(crate) mod auxiliary_completion_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) async fn test_app(url: &str) -> App {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','test','unused')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','test',0,1)")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,?)")
            .bind(url)
            .execute(&db)
            .await
            .unwrap();
        App::new(db, test_adapter_host(), PathBuf::new(), [7; 32])
    }

    #[tokio::test]
    async fn auxiliary_completions_use_the_selected_native_plugin() {
        let listener = crate::test_net::mock_listener().await;
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            let header_end = bytes
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + length {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            assert!(bytes.starts_with(b"POST /api/chat HTTP/1.1\r\n"));
            let body = r#"{"message":{"content":"converted"},"done_reason":"stop","prompt_eval_count":5,"eval_count":2}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let app = test_app(&url).await;
        sqlx::query("UPDATE broker_settings SET use_ollama_api=1")
            .execute(&app.db)
            .await
            .unwrap();
        let response = complete(
            &app,
            "model",
            vec![AdapterMessage::new("user", "convert")],
            GenerationOptions {
                max_tokens: Some(128),
                ..Default::default()
            },
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(response.content.as_deref(), Some("converted"));
        assert_eq!(response.usage.prompt_tokens, 5);
        assert_eq!(response.usage.completion_tokens, 2);
        server.await.unwrap();
    }
}

mod speaker_tests {
    use super::auxiliary_completion_tests::test_app;
    use super::*;

    const PARTICIPANTS: &str = "SELECT c.id,c.name FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id='chat' ORDER BY p.position";

    /// The shared test chat as an automatic group of Ann then Bob. The adapter
    /// URL refuses connections, so any model-based choice falls back.
    async fn group_app() -> App {
        let app = test_app("http://127.0.0.2:9/v1").await;
        sqlx::query("INSERT INTO characters(id,owner_id,name,personality,scenario,system_prompt,example_dialogue,appearance,tags,revision) VALUES('ann','owner','Ann','','','','','',X'',1),('bob','owner','Bob','','','','','',X'',1)")
            .execute(&app.db).await.unwrap();
        sqlx::query("INSERT INTO participants(conversation_id,character_id,position) VALUES('chat','ann',0),('chat','bob',1)")
            .execute(&app.db).await.unwrap();
        sqlx::query("UPDATE conversations SET kind=? WHERE id='chat'")
            .bind(ConversationKind::GroupAutomatic as i32)
            .execute(&app.db)
            .await
            .unwrap();
        app
    }

    async fn speaker(app: &App) -> String {
        let participants = sqlx::query(PARTICIPANTS).fetch_all(&app.db).await.unwrap();
        select_speaker(app, "owner", "chat", None, &participants)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn automatic_groups_answer_mentions_and_never_self_reply() {
        let app = group_app().await;
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,content,revision) VALUES('u1','chat','user','What do you see, bob?',2)")
            .execute(&app.db).await.unwrap();
        assert_eq!(speaker(&app).await, "bob");
        // Bob naming himself does not make him answer his own line.
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES('b1','chat','character','bob','Bob squints at the sky.',3)")
            .execute(&app.db).await.unwrap();
        assert_eq!(speaker(&app).await, "ann");
        // With no usable mention, round-robin still skips the latest author.
        sqlx::query("UPDATE conversations SET turn_index=1 WHERE id='chat'")
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(speaker(&app).await, "ann");
    }

    #[tokio::test]
    async fn group_history_names_other_speakers() {
        let app = group_app().await;
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES('u1','chat','user',NULL,'Hello',2),('a1','chat','character','ann','Hi there',3),('b1','chat','character','bob','Evening',4)")
            .execute(&app.db).await.unwrap();
        let rows = sqlx::query(PROMPT_HISTORY)
            .bind("chat")
            .fetch_all(&app.db)
            .await
            .unwrap();
        let history: Vec<_> = rows
            .iter()
            .map(|r| history_message(r, Some("bob")))
            .collect();
        assert_eq!(
            history,
            [
                ("assistant".to_owned(), "Evening".to_owned()),
                ("user".to_owned(), "Ann: Hi there".to_owned()),
                ("user".to_owned(), "User: Hello".to_owned()),
            ]
        );
        // Ann's and the user's lines become one user turn, oldest first.
        assert_eq!(
            merge_same_role_turns(history),
            [
                ("assistant".to_owned(), "Evening".to_owned()),
                ("user".to_owned(), "User: Hello\n\nAnn: Hi there".to_owned()),
            ]
        );
        let participants = sqlx::query(
            "SELECT c.id,c.name,c.description,c.personality,c.appearance FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id='chat' ORDER BY p.position",
        )
        .fetch_all(&app.db)
        .await
        .unwrap();
        let prompt = group_prompt(&participants, "bob", "Bob");
        assert!(prompt.contains("- Ann:") && !prompt.contains("- Bob:"));
        assert!(prompt.contains("Write only Bob's next reply"));
        assert!(group_prompt(&participants[..1], "ann", "Ann").is_empty());
    }

    #[test]
    fn matches_whole_names_only() {
        assert_eq!(name_mention("What do you think, mira?", "Mira"), Some(19));
        assert_eq!(name_mention("Admiral Ann", "Ann"), Some(8));
        assert_eq!(name_mention("Miranda waves", "Mira"), None);
        assert_eq!(name_mention("Hannah", "Ann"), None);
        assert_eq!(name_mention("anything", " "), None);
    }

    fn bob_filter() -> SpeakerFilter {
        SpeakerFilter {
            own: "Bob".into(),
            others: vec!["Ann".into(), GROUP_USER_NAME.into()],
        }
    }

    /// Stream `chunks` through a group reply and return what the client saw
    /// and what is stored.
    fn stream(chunks: &[&str]) -> (String, String) {
        let mut reply = ReplyText::new(Some(bob_filter()));
        let mut shown = String::new();
        for chunk in chunks {
            reply.push(chunk);
            shown.push_str(&reply.take_pending());
            if reply.stopped() {
                break;
            }
        }
        reply.finish();
        shown.push_str(&reply.take_pending());
        (shown, reply.content().to_owned())
    }

    #[test]
    fn name_prefixes_ignore_case_and_emphasis() {
        assert_eq!(name_prefix("ann: hi", "Ann"), NamePrefix::Yes(4));
        assert_eq!(name_prefix("**Ann:** hi", "Ann"), NamePrefix::Yes(6));
        assert_eq!(name_prefix("**An", "Ann"), NamePrefix::Undecided);
        assert_eq!(name_prefix("Ann", "Ann"), NamePrefix::Undecided);
        assert_eq!(name_prefix("Ann waves", "Ann"), NamePrefix::No);
        assert_eq!(name_prefix("Annabel: hi", "Ann"), NamePrefix::No);
        assert_eq!(name_prefix("*Ann laughs*", "Ann"), NamePrefix::No);
    }

    #[test]
    fn group_replies_stop_before_another_speakers_line() {
        // The opening name is dropped; Ann's line and later text never show.
        let (shown, stored) = stream(&[
            "Bo",
            "b: Evening.",
            "\nIt is cold.\n",
            "An",
            "n: Hi Bob!\nUser: hey",
        ]);
        assert_eq!(shown, "Evening.\nIt is cold.");
        assert_eq!(stored, "Evening.\nIt is cold.");
        // An own-name prefix followed by a newline still rejects another speaker's line.
        let (shown, stored) = stream(&["Bob:\n", "Ann: hi"]);
        assert_eq!(shown, "");
        assert_eq!(stored, "");
        // So does a line for the user.
        let (_, stored) = stream(&["Sure.\n\n**User:** thanks"]);
        assert_eq!(stored, "Sure.");
        // Mentions and narration that only start like a name are kept.
        let (shown, stored) = stream(&["I look at Ann.\nAnn", " smiles.\n*Ann waves*"]);
        assert_eq!(shown, "I look at Ann.\nAnn smiles.\n*Ann waves*");
        assert_eq!(stored, shown);
    }

    #[tokio::test]
    async fn automatic_speaker_selection_skips_model_when_one_candidate_remains() {
        let app = group_app().await;
        sqlx::query("UPDATE broker_settings SET adapter_enabled=0")
            .execute(&app.db)
            .await
            .unwrap();
        let participants = sqlx::query(PARTICIPANTS).fetch_all(&app.db).await.unwrap();
        assert_eq!(
            choose_speaker_with_model(&app, "owner", &participants, &[], Some("bob"))
                .await
                .as_deref(),
            Some("ann")
        );
    }

    #[test]
    fn undecided_text_is_released_when_the_reply_ends() {
        let (shown, stored) = stream(&["Fine.\nAn"]);
        assert_eq!(shown, "Fine.\nAn");
        assert_eq!(stored, "Fine.\nAn");
        let (shown, _) = stream(&["Bo"]);
        assert_eq!(shown, "Bo");
        // The first line is never cut, so the reply cannot become empty.
        let (_, stored) = stream(&["Ann: hello"]);
        assert_eq!(stored, "Ann: hello");
    }

    #[test]
    fn direct_replies_are_not_filtered() {
        let mut reply = ReplyText::new(None);
        reply.push("Hi.\nUser: more  ");
        reply.finish();
        assert_eq!(reply.take_pending(), "Hi.\nUser: more  ");
        assert_eq!(reply.content(), "Hi.\nUser: more  ");
    }
}

mod world_import_tests {
    use super::*;

    #[test]
    fn normalizes_legacy_and_character_book_entries() {
        let legacy = json!({"entries":{"0":{"comment":"Sky","content":"Two moons","key":["moon"],"constant":true,"disable":true,"order":250}}});
        let entries = normalized_silly_tavern_entries(&legacy).unwrap();
        assert_eq!(entries[0]["memo"], "Sky");
        assert_eq!(entries[0]["enabled"], false);
        assert_eq!(entries[0]["order"], 250);

        let card = json!({"data":{"character_book":{"entries":[{"comment":"Gate","content":"Closed at dusk","keys":["gate"],"secondary_keys":["north"],"enabled":true,"insertion_order":80}]}}});
        let entries = normalized_silly_tavern_entries(&card).unwrap();
        assert_eq!(entries[0]["primary_keys"][0], "gate");
        assert_eq!(entries[0]["secondary_keys"][0], "north");
    }

    #[test]
    fn accepts_a_fenced_valid_preview_and_rejects_invalid_facts() {
        let world = parse_world_import(
            "```json\n{\"name\":\"Realm\",\"entries\":[{\"title\":\"Gate\",\"content\":\"Closed\",\"keywords\":[\" gate \",\"gate\"],\"common_knowledge\":false,\"enabled\":true,\"priority\":2500}]}\n```",
            "fallback",
        ).unwrap();
        assert_eq!(world.entries[0].keywords, ["gate"]);
        assert_eq!(world.entries[0].priority, 1000);
        assert!(parse_world_import(
            r#"{"name":"Realm","entries":[{"title":"Gate","content":"Closed","keywords":[],"common_knowledge":false,"enabled":true,"priority":1}]}"#,
            "fallback",
        ).is_err());
    }
}
