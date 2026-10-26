//! Exercise the compiled FreeToken guest through the production Wasmi/HTTP host.
use super::tests::{plan, read_request, registry, request, session};
use super::*;
use serde_json::json;

#[test]
fn freetoken_plans_only_send_supported_options() {
    for stream in [false, true] {
        let mut chat = request();
        chat.options.disable_reasoning = true;
        let request = if stream {
            AdapterRequest::Stream(chat)
        } else {
            AdapterRequest::Complete(chat)
        };
        let plan = plan("freetoken", request);
        let call = &plan.requests[0];
        assert_eq!(
            call.url,
            "http://inference.example:11434/v1/chat/completions"
        );
        assert_eq!(call.timeout_ms, Some(60_000));
        assert_eq!(plan.stream, stream);
        let body = call.body.as_ref().unwrap();
        assert_eq!(body["model"], "dynamic-model-id");
        assert_eq!(body["messages"], json!([{"role":"user","content":"Hello"}]));
        assert_eq!(body["temperature"], json!(0.4_f32));
        assert_eq!(body["top_p"], json!(0.9_f32));
        assert_eq!(body["top_k"], 40);
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["stream"], stream);
        if stream {
            assert_eq!(body["stream_options"]["include_usage"], true);
        }
        for unsupported in [
            "seed",
            "repeat_penalty",
            "context_tokens",
            "keep_alive",
            "options",
        ] {
            assert!(body.get(unsupported).is_none(), "{unsupported}");
        }
    }
    let mut chat = request();
    chat.options.max_tokens = Some(-1);
    let plan = plan("freetoken", AdapterRequest::Complete(chat));
    let body = plan.requests[0].body.as_ref().unwrap();
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());
    for base in [
        "http://inference.example",
        "http://inference.example/",
        "http://inference.example/v1/",
        "http://inference.example/proxy/v1",
    ] {
        let adapter = registry().adapter("freetoken", base).unwrap();
        let plan = adapter
            .prepare(
                &mut session("freetoken"),
                AdapterRequest::Models { timeout_ms: 900 },
            )
            .unwrap();
        let expected = if base.contains("proxy") {
            "http://inference.example/proxy/v1/models"
        } else {
            "http://inference.example/v1/models"
        };
        assert_eq!(plan.requests[0].url, expected);
    }
}

#[tokio::test]
async fn freetoken_lifecycle_and_catalogue_fail_before_transport() {
    let adapter = registry()
        .adapter("freetoken", "http://127.0.0.1:1/v1")
        .unwrap();
    for (action, operation) in [
        (ModelAction::Pull { model: "m".into() }, "download"),
        (ModelAction::Delete { model: "m".into() }, "delete"),
        (
            ModelAction::Load {
                model: "m".into(),
                keep_alive: "5m".into(),
            },
            "load",
        ),
        (ModelAction::Unload { model: "m".into() }, "unload"),
    ] {
        let error = adapter.model_action(action).await.unwrap_err().to_string();
        assert!(
            error.contains(&format!("{operation} unsupported")),
            "{error}"
        );
    }
    assert!(
        adapter
            .search("m")
            .await
            .unwrap_err()
            .to_string()
            .contains("catalogue search unsupported")
    );
}

#[test]
fn freetoken_fragmented_sse_keeps_reasoning_out_and_preserves_usage_after_finish() {
    let bytes = concat!(
        ": keepalive\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"secret thought\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hé🙂\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":19,\"completion_tokens\":8}}\n\n",
        "data: [DONE]" // Host supplies a newline at EOF.
    )
    .as_bytes();
    let mut decoder = Decoder {
        session: session("freetoken"),
        buffer: Vec::new(),
    };
    let mut text = String::new();
    let mut finish = None;
    let mut prompt = None;
    let mut completion = None;
    for byte in bytes {
        if let Some(update) = decoder.push(&[*byte], false).unwrap() {
            assert!(!update.done);
            text.push_str(&update.text);
            finish = update.finish_reason.or(finish);
            prompt = update.prompt_tokens.or(prompt);
            completion = update.completion_tokens.or(completion);
        }
    }
    assert!(decoder.push(&[], true).unwrap().unwrap().done);
    assert_eq!(text, "hé🙂");
    assert_eq!(finish.as_deref(), Some("length"));
    assert_eq!(prompt, Some(19));
    assert_eq!(completion, Some(8));
    for reason in ["stop", "length", "tool_calls"] {
        let PluginOutput::Completion(value) = session("freetoken").invoke(&PluginInput::Decode {
            kind: ResponseKind::Completion,
            responses: vec![json!({"choices":[{"message":{"content":null,"reasoning_content":"private","thinking":"private"},"finish_reason":reason}],"usage":{"prompt_tokens":4,"completion_tokens":6}})],
        }).unwrap() else { panic!("completion expected") };
        assert!(value.content.is_none());
        assert_eq!(value.finish_reason.as_deref(), Some(reason));
        assert_eq!(
            value.usage,
            TokenUsage {
                prompt_tokens: 4,
                completion_tokens: 6
            }
        );
    }
    // Multiple records in one network batch retain the last counters and completion.
    let mut decoder = Decoder {
        session: session("freetoken"),
        buffer: Vec::new(),
    };
    let update = decoder.push(bytes, true).unwrap().unwrap();
    assert!(update.done);
    assert_eq!(update.text, "hé🙂");
    assert_eq!(update.completion_tokens, Some(8));
}

#[test]
fn freetoken_errors_and_health_are_normalized() {
    for (bytes, expected) in [
        (
            "data: {\"error\":{\"message\":\"context exceeded\"}}\n",
            "context exceeded",
        ),
        (
            "data: {\"error\":\"generation failed\"}\n",
            "generation failed",
        ),
        ("data: invalid\n", "malformed"),
    ] {
        let mut decoder = Decoder {
            session: session("freetoken"),
            buffer: Vec::new(),
        };
        assert!(
            decoder
                .push(bytes.as_bytes(), false)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    let error = session("freetoken")
        .invoke(&PluginInput::Decode {
            kind: ResponseKind::Completion,
            responses: vec![json!({"error":{"message":"bad input"}})],
        })
        .unwrap_err();
    assert!(error.to_string().contains("bad input"));
    for (body, count) in [
        (
            json!({"status":"ok","maintenance":"serving","model":"served","version":"1.2"}),
            1,
        ),
        (json!({"status":"loading","model":"served"}), 0),
        (
            json!({"status":"ok","maintenance":"rebuild","model":"served","version":"1.2"}),
            0,
        ),
    ] {
        let PluginOutput::State(state) = session("freetoken")
            .invoke(&PluginInput::Decode {
                kind: ResponseKind::State,
                responses: vec![body.clone()],
            })
            .unwrap()
        else {
            panic!("state expected")
        };
        assert_eq!(state.running_models.len(), count);
        assert!(state.models.is_empty());
        assert_eq!(state.version, body["version"].as_str().unwrap_or("unknown"));
        if count > 0 {
            assert_eq!(state.running_models[0].name, "served");
        }
    }
    for body in [
        json!({"status":"error","message":"load failed"}),
        json!({}),
        json!({"status":"unrecognized"}),
    ] {
        assert!(
            session("freetoken")
                .invoke(&PluginInput::Decode {
                    kind: ResponseKind::State,
                    responses: vec![body]
                })
                .is_err()
        );
    }
}

#[tokio::test]
async fn freetoken_compiled_plugin_uses_remote_http_for_discovery_completion_and_state() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for (path, response) in [
            (
                "GET /v1/models",
                json!({"data":[{"id":"remote-model"},{"id":"another-model"}]}),
            ),
            (
                "POST /v1/chat/completions",
                json!({"choices":[{"message":{"content":"final answer","reasoning_content":"private"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":9}}),
            ),
            (
                "GET /health",
                json!({"status":"ok","maintenance":"serving","model":"remote-model","version":"test"}),
            ),
        ] {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut socket, first, body) = read_request(socket).await;
            assert!(first.starts_with(path), "{first}");
            if body.is_object() {
                assert_eq!(body["stream"], false);
            }
            let body = response.to_string();
            socket
                .get_mut()
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let adapter = registry().adapter("freetoken", &url).unwrap();
    assert_eq!(
        adapter.models(Duration::from_secs(2)).await.unwrap(),
        ["remote-model", "another-model"]
    );
    let result = adapter.complete(request()).await.unwrap();
    assert_eq!(result.content.as_deref(), Some("final answer"));
    assert_eq!(result.usage.completion_tokens, 9);
    assert_eq!(
        adapter.model_state().await.unwrap().running_models[0].name,
        "remote-model"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn freetoken_credentials_are_host_owned_origin_pinned_and_redirect_safe() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    // Synthetic test credential, never a real server key.
    let token = "synthetic-test-credential";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let other_origin = format!("http://{}", other.local_addr().unwrap());
    let adapter = registry()
        .adapter("freetoken", &format!("{origin}/v1"))
        .unwrap()
        .with_bearer_token(&origin, token)
        .unwrap();
    assert!(
        adapter
            .clone()
            .with_bearer_token(&other_origin, token)
            .is_err()
    );
    assert!(
        adapter
            .clone()
            .with_bearer_token(&format!("{origin}/v1"), token)
            .is_err()
    );
    assert!(
        adapter
            .clone()
            .with_bearer_token(&origin, "bad\nheader")
            .is_err()
    );
    assert!(
        adapter
            .clone()
            .with_bearer_token(&origin, "non-ascii-é")
            .is_err()
    );
    let plan = adapter
        .prepare(
            &mut session("freetoken"),
            AdapterRequest::Models { timeout_ms: 1000 },
        )
        .unwrap();
    assert!(!serde_json::to_string(&plan).unwrap().contains(token));
    // Even a direct transport call cannot attach the key to another origin.
    assert!(
        adapter
            .send(HttpRequest::get(format!("{other_origin}/models"), 1000))
            .await
            .is_err()
    );
    let redirect = other_origin.clone();
    let task = tokio::spawn(async move {
        for status in [302, 401, 200] {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(socket);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert_eq!(
                headers.contains(&format!("authorization: Bearer {token}\r\n")),
                status != 200
            );
            let body = if status == 401 {
                json!({"error":format!("invalid key {token}")})
            } else {
                json!({"data":[]})
            }
            .to_string();
            reader.get_mut().write_all(format!("HTTP/1.1 {status} Test\r\nLocation: {redirect}/models\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let error = adapter
        .models(Duration::from_secs(2))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("redirect"));
    let error = adapter
        .models(Duration::from_secs(2))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("401"));
    assert!(error.contains("[redacted]"));
    assert!(!error.contains(token));
    // A second adapter on the same pooled client does not inherit the header.
    registry()
        .adapter("openai-compatible", &format!("{origin}/v1"))
        .unwrap()
        .models(Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), other.accept())
            .await
            .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn freetoken_authenticated_streams_handle_fragmentation_and_redact_errors() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "synthetic-stream-credential";
    let task = tokio::spawn(async move {
        for response_kind in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(socket);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(headers.contains(&format!("authorization: Bearer {token}\r\n")));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["stream"], response_kind != 2);
            let error = json!({"error":{"message":format!("provider echoed {token}")}}).to_string();
            if response_kind == 2 {
                reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",error.len()).as_bytes()).await.unwrap();
                continue;
            }
            reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            let records = if response_kind == 0 {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"private\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hé🙂\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3},\"choices\":[]}\n\n",
                    "data: [DONE]\n\n"
                ).to_owned()
            } else {
                format!("data: {error}\n\ndata: [DONE]\n\n")
            };
            for fragment in records.as_bytes().chunks(2) {
                reader.get_mut().write_all(fragment).await.unwrap();
                tokio::task::yield_now().await;
            }
        }
    });
    let adapter = registry()
        .adapter("freetoken", &origin)
        .unwrap()
        .with_bearer_token(&origin, token)
        .unwrap();
    let mut stream = adapter.stream(request()).await.unwrap();
    let mut text = String::new();
    let mut finish = None;
    let mut usage = None;
    let mut done = false;
    while let Some(update) = stream.next().await.unwrap() {
        text.push_str(&update.text);
        finish = update.finish_reason.or(finish);
        usage = update.completion_tokens.or(usage);
        done |= update.done;
    }
    assert_eq!(text, "hé🙂");
    assert_eq!(finish.as_deref(), Some("stop"));
    assert_eq!(usage, Some(3));
    assert!(done);
    let mut stream = adapter.stream(request()).await.unwrap();
    let error = loop {
        match stream.next().await {
            Err(error) => break error.to_string(),
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected stream error"),
        }
    };
    assert!(error.contains("[redacted]"));
    assert!(!error.contains(token));
    let error = adapter.complete(request()).await.unwrap_err().to_string();
    assert!(error.contains("[redacted]"));
    assert!(!error.contains(token));
    task.await.unwrap();
}
