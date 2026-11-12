//! Administrator configuration, logs, and model management.
use super::*;

#[test]
fn admin_log_reader_returns_the_newest_complete_bounded_lines() {
    let path = std::env::temp_dir().join(format!("chatty-admin-log-{}.log", new_uuid()));
    let mut file = File::create(&path).unwrap();
    file.write_all(&vec![b'x'; MAX_ADMIN_LOG_RESPONSE_BYTES as usize])
        .unwrap();
    file.write_all(b"\nnewest complete line\n").unwrap();
    drop(file);
    let log = read_broker_log(&path).unwrap();
    assert!(log.truncated);
    assert!(log.content.ends_with("newest complete line\n"));
    assert!(!log.content.starts_with('x'));
    assert!(log.content.len() <= MAX_ADMIN_LOG_RESPONSE_BYTES as usize);
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn runtime_adapter_selection_is_authorized_validated_and_persisted() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,use_ollama_api) VALUES(1,'http://inference.example:11434',1)")
        .execute(&db).await.unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let mut tokens = Vec::new();
    for username in ["adapter-admin", "adapter-user"] {
        let (_, payload) = call(
            &app,
            Request::Register {
                username: username.into(),
                password: "test-password".into(),
            },
        )
        .await
        .unwrap();
        let Response::Authenticated { session_token, .. } = decode::<Response>(&payload).unwrap()
        else {
            panic!("registration failed")
        };
        tokens.push(session_token);
    }
    let mut config = load_broker_config(&app.db).await.unwrap();
    assert_eq!(
        config.adapter_plugin_id(),
        "ollama",
        "legacy setting is preserved"
    );
    let (_, payload) = call(
        &app,
        Request::AdminGetBrokerConfig {
            session_token: tokens[0].clone(),
        },
    )
    .await
    .unwrap();
    let Response::BrokerConfig {
        adapter_plugins,
        adapter_override,
        ..
    } = decode::<Response>(&payload).unwrap()
    else {
        panic!("missing adapter choices")
    };
    assert_eq!(
        adapter_plugins,
        ["freetoken", "ollama", "openai-compatible"]
    );
    assert!(adapter_override.is_none());
    assert!(
        call(
            &app,
            Request::AdminGetBrokerConfig {
                session_token: tokens[1].clone()
            }
        )
        .await
        .is_err()
    );
    config.adapter_plugin = "freetoken".into();
    assert!(
        call(
            &app,
            Request::AdminSetBrokerConfig {
                session_token: tokens[1].clone(),
                config: Box::new(config.clone()),
            }
        )
        .await
        .is_err()
    );
    let (_, payload) = call(
        &app,
        Request::AdminSetBrokerConfig {
            session_token: tokens[0].clone(),
            config: Box::new(config.clone()),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(decode::<Response>(&payload).unwrap(), Response::BrokerConfig { config, .. }
        if config.adapter_plugin == "freetoken")
    );
    assert_eq!(
        load_broker_config(&app.db)
            .await
            .unwrap()
            .adapter_plugin_id(),
        "freetoken"
    );
    for id in ["not-installed", "hugging-face"] {
        config.adapter_plugin = id.into();
        assert!(
            call(
                &app,
                Request::AdminSetBrokerConfig {
                    session_token: tokens[0].clone(),
                    config: Box::new(config.clone()),
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            load_broker_config(&app.db).await.unwrap().adapter_plugin,
            "freetoken"
        );
    }
}

#[tokio::test]
async fn model_pull_is_sent_to_the_configured_inference_server() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let server = crate::test_net::mock_listener().await;
    let address = server.local_addr().unwrap();
    let receiver = tokio::spawn(async move {
        let (socket, _) = server.accept().await.unwrap();
        let mut socket = tokio::io::BufReader::new(socket);
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        assert_eq!(line, "POST /api/pull HTTP/1.1\r\n");
        let mut length = 0;
        loop {
            line.clear();
            socket.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], "hf.co/owner/chat-GGUF:chat-Q4_K_M.gguf");
        assert_eq!(body["stream"], false);
        socket
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"status\":\"success\"}")
            .await
            .unwrap();
    });
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,use_ollama_api) VALUES(1,?,1)")
        .bind(format!("http://{address}/v1"))
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    tokio::time::timeout(Duration::from_secs(5), async {
        run_ollama_action(
            &app,
            OllamaAction::Pull {
                model: "hf.co/owner/chat-GGUF:chat-Q4_K_M.gguf".into(),
            },
        )
        .await
        .unwrap();
        receiver.await.unwrap();
    })
    .await
    .unwrap();
}
