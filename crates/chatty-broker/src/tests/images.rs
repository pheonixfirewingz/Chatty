//! Image generation, shared-GPU scheduling, and workflows.
use super::*;

#[tokio::test]
async fn scene_images_are_generated_encrypted_and_owner_scoped() {
    let (url, seen) = mock_comfyui(test_portrait_png()).await;
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,'http://inference.example:11434/v1')")
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let mut tokens = Vec::new();
    for username in ["image-admin", "image-other"] {
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
    let owner = auth(&app.db, &tokens[0]).await.unwrap();
    sqlx::query(
        "INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat',?,'Chat',0,1)",
    )
    .bind(&owner)
    .execute(&app.db)
    .await
    .unwrap();
    let scene = Request::GenerateSceneImage {
        session_token: tokens[0].clone(),
        conversation_id: "chat".into(),
        prompt: "a lighthouse at dusk".into(),
    };
    let error = call(&app, scene.clone()).await.unwrap_err();
    assert!(error.to_string().contains("disabled"));

    let mut config = load_broker_config(&app.db).await.unwrap();
    config.image.enabled = true;
    config.image.url = url;
    config.image.width = 512;
    config.image.height = 768;
    let denied = call(
        &app,
        Request::AdminSetBrokerConfig {
            session_token: tokens[1].clone(),
            config: Box::new(config.clone()),
        },
    )
    .await;
    assert!(denied.is_err(), "only administrators configure images");
    let (kind, _) = call(
        &app,
        Request::AdminSetBrokerConfig {
            session_token: tokens[0].clone(),
            config: Box::new(config),
        },
    )
    .await
    .unwrap();
    assert_eq!(kind, MessageType::Response);
    let (_, payload) = call(
        &app,
        Request::AdminListImageModels {
            session_token: tokens[0].clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(decode::<Response>(&payload).unwrap(), Response::ImageModels(models) if models == ["model.safetensors", "flux1-dev.safetensors"])
    );

    let (_, payload) = call(&app, scene).await.unwrap();
    let Response::Accepted {
        entity_id: Some(message_id),
        ..
    } = decode::<Response>(&payload).unwrap()
    else {
        panic!("scene image was not accepted")
    };
    let (author_type, content): (String, String) =
        sqlx::query_as("SELECT author_type,content FROM messages WHERE id=?")
            .bind(&message_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(
        (author_type.as_str(), content.as_str()),
        ("image", "a lighthouse at dusk")
    );
    let stored: Vec<u8> = sqlx::query_scalar("SELECT data FROM message_images WHERE message_id=?")
        .bind(&message_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(stored.starts_with(b"CHIMG1"));

    let (_, payload) = call(
        &app,
        Request::GetMessageImage {
            session_token: tokens[0].clone(),
            message_id: message_id.clone(),
        },
    )
    .await
    .unwrap();
    let Response::MessageImage { data, .. } = decode::<Response>(&payload).unwrap() else {
        panic!("owner could not read the image")
    };
    assert_eq!(
        image::load_from_memory(&data)
            .unwrap()
            .into_rgb8()
            .dimensions(),
        (128, 128)
    );
    let denied = call(
        &app,
        Request::GetMessageImage {
            session_token: tokens[1].clone(),
            message_id,
        },
    )
    .await;
    assert!(denied.is_err(), "other tenants cannot read images");

    let (_, payload) = call(
        &app,
        Request::GenerateImage {
            session_token: tokens[1].clone(),
            prompt: "portrait of a knight".into(),
            negative_prompt: "blurry".into(),
            square: true,
            seed: Some(42),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        decode::<Response>(&payload).unwrap(),
        Response::GeneratedImage { .. }
    ));
    let seen = seen.lock().unwrap();
    let workflows = seen
        .iter()
        .filter(|(request, _)| request == "POST /prompt")
        .map(|(_, body)| &body["prompt"])
        .collect::<Vec<_>>();
    assert_eq!(workflows[0]["4"]["inputs"]["width"], 512);
    assert_eq!(workflows[0]["4"]["inputs"]["height"], 768);
    assert_eq!(
        workflows[1]["4"]["inputs"]["height"], 512,
        "portraits are square"
    );
    assert_eq!(workflows[1]["3"]["inputs"]["text"], "blurry");
    assert_eq!(
        workflows[1]["5"]["inputs"]["seed"], 42,
        "a requested seed is kept"
    );
    assert_eq!(
        workflows[1]["1"]["inputs"]["ckpt_name"],
        "model.safetensors"
    );
}

#[tokio::test]
async fn shared_gpu_host_unloads_the_other_model_kind_before_switching() {
    let (url, seen) = mock_comfyui(test_portrait_png()).await;
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,adapter_plugin,image_enabled,image_url) VALUES(1,?,'ollama',1,?)")
        .bind(&url)
        .bind(&url)
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let requests = |seen: &Arc<std::sync::Mutex<Vec<(String, Value)>>>| {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(request, body)| (request.clone(), body.clone()))
            .collect::<Vec<_>>()
    };
    generate_png(&app, "a harbour", "", false, None)
        .await
        .unwrap();
    let log = requests(&seen);
    let unload = log
        .iter()
        .position(|(request, body)| request == "POST /api/generate" && body["keep_alive"] == 0)
        .expect("running text model is unloaded");
    let submit = log
        .iter()
        .position(|(request, _)| request == "POST /prompt")
        .unwrap();
    assert!(
        unload < submit,
        "text is unloaded before the image job starts"
    );
    assert_eq!(log[unload].1["model"], "chat-model");

    let messages = vec![AdapterMessage::new("user", "hello")];
    for _ in 0..2 {
        complete(
            &app,
            "chat-model",
            messages.clone(),
            GenerationOptions::default(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    }
    let log = requests(&seen);
    let free = log
        .iter()
        .filter(|(request, _)| request == "POST /free")
        .collect::<Vec<_>>();
    assert_eq!(free.len(), 1, "only the switch to text frees image memory");
    assert_eq!(free[0].1["unload_models"], true);
    let free_at = log
        .iter()
        .position(|(request, _)| request == "POST /free")
        .unwrap();
    let chat_at = log
        .iter()
        .position(|(request, _)| request == "POST /api/chat")
        .unwrap();
    assert!(free_at < chat_at);
}

#[tokio::test]
async fn separate_hosts_do_not_unload_models() {
    let (url, seen) = mock_comfyui(test_portrait_png()).await;
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,adapter_plugin,image_enabled,image_url) VALUES(1,'http://inference.example:11434','ollama',1,?)")
        .bind(&url)
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    generate_png(&app, "a harbour", "", false, None)
        .await
        .unwrap();
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("GET /api/") || request == "POST /free")
    );
}

#[tokio::test]
async fn image_job_is_refused_while_the_chat_model_stays_loaded() {
    let (url, seen) = mock_comfyui_with_ollama(test_portrait_png(), false).await;
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,adapter_plugin,image_enabled,image_url) VALUES(1,?,'ollama',1,?)")
        .bind(&url)
        .bind(&url)
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    let error = generate_png(&app, "a harbour", "", false, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("still loaded"), "{error}");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request == "POST /prompt"),
        "the image server never receives the job"
    );
}

#[test]
fn image_workflow_templates_are_validated_before_saving() {
    let valid = r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}}}"#;
    assert!(validate_image_workflow("").is_ok());
    assert!(validate_image_workflow(valid).is_ok());
    for (workflow, message) in [
        ("not json", "not valid JSON"),
        (r#"{"nodes": [], "links": []}"#, "Export (API)"),
        (r#"{"1": {"inputs": {"text": "{{prompt}}"}}}"#, "class_type"),
        (
            r#"{"1": {"class_type": "SaveImage", "inputs": {}}}"#,
            "{{prompt}}",
        ),
        (
            r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}} {{lora}}"}}}"#,
            "{{lora}}",
        ),
    ] {
        let error = validate_image_workflow(workflow).unwrap_err().to_string();
        assert!(error.contains(message), "{workflow}: {error}");
    }
}

#[tokio::test]
async fn custom_image_workflows_are_sent_with_filled_placeholders() {
    let (url, seen) = mock_comfyui(test_portrait_png()).await;
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    let workflow = r#"{
        "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "flux1-dev.safetensors", "weight_dtype": "default"}},
        "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}},
        "3": {"class_type": "EmptySD3LatentImage", "inputs": {"width": "{{width}}", "height": "{{height}}", "batch_size": 1}},
        "4": {"class_type": "SaveImage", "inputs": {"filename_prefix": "chatty"}}
    }"#;
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,image_enabled,image_url,image_model,image_width,image_height,image_workflow) VALUES(1,'http://inference.example:11434/v1',1,?,'not-installed',768,512,?)")
        .bind(&url)
        .bind(workflow)
        .execute(&db)
        .await
        .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);
    generate_png(&app, "a harbour", "", false, Some(5))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        !seen
            .iter()
            .any(|(request, _)| request.starts_with("GET /models/")),
        "a template without {{{{model}}}} skips model lookup"
    );
    let graph = &seen
        .iter()
        .find(|(request, _)| request == "POST /prompt")
        .unwrap()
        .1["prompt"];
    assert_eq!(graph["1"]["class_type"], "UNETLoader");
    assert_eq!(graph["2"]["inputs"]["text"], "a harbour");
    assert_eq!(graph["3"]["inputs"]["width"], 768);
    assert_eq!(graph["3"]["inputs"]["height"], 512);
}
