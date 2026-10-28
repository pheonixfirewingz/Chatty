use super::*;
use serde_json::json;
use std::sync::OnceLock;

pub(super) fn registry() -> Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Registry::bundled().expect("bundled WASM plugins load"))
        .clone()
}
pub(super) fn session(id: &str) -> Session {
    registry().load_plugin(id).unwrap().session().unwrap()
}
pub(super) fn plan(id: &str, request: AdapterRequest) -> HttpPlan {
    let adapter = registry()
        .adapter(id, "http://inference.example:11434/v1")
        .unwrap();
    adapter.prepare(&mut session(id), request).unwrap()
}
pub(super) fn request() -> ChatRequest {
    ChatRequest {
        model: "dynamic-model-id".into(),
        messages: vec![ChatMessage::new("user", "Hello")],
        options: GenerationOptions {
            temperature: Some(0.4),
            top_p: Some(0.9),
            top_k: Some(40),
            context_tokens: Some(8192),
            max_tokens: Some(128),
            repeat_penalty: Some(1.1),
            seed: Some(7),
            keep_alive: Some("5m".into()),
            disable_reasoning: false,
            reasoning_effort: None,
        },
        timeout_ms: Some(60_000),
    }
}

#[test]
fn real_wasm_plugins_translate_chat_requests() {
    let openai = plan("openai-compatible", AdapterRequest::Stream(request()));
    assert_eq!(
        openai.requests[0].url,
        "http://inference.example:11434/v1/chat/completions"
    );
    let body = openai.requests[0].body.as_ref().unwrap();
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_tokens"], 128);
    assert!(body.get("options").is_none());
    let native = plan("ollama", AdapterRequest::Stream(request()));
    assert_eq!(
        native.requests[0].url,
        "http://inference.example:11434/api/chat"
    );
    let body = native.requests[0].body.as_ref().unwrap();
    assert_eq!(body["options"]["num_ctx"], 8192);
    assert_eq!(body["options"]["num_predict"], 128);
    assert_eq!(body["keep_alive"], "5m");
    let mut classification = request();
    classification.options.disable_reasoning = true;
    assert_eq!(
        plan("ollama", AdapterRequest::Complete(classification.clone())).requests[0]
            .body
            .as_ref()
            .unwrap()["think"],
        false
    );
    assert_eq!(
        plan(
            "openai-compatible",
            AdapterRequest::Complete(classification)
        )
        .requests[0]
            .body
            .as_ref()
            .unwrap()["reasoning_effort"],
        "none"
    );
}

#[test]
fn lifecycle_plans_target_the_inference_server() {
    assert!(registry().contains("openai"));
    assert!(
        registry()
            .adapter("openai", "http://inference.example/v1")
            .is_ok()
    );

    let model = "hf.co/owner/repo:model-Q4_K_M.gguf";
    for (action, path, method) in [
        (
            ModelAction::Pull {
                model: model.into(),
            },
            "pull",
            "Post",
        ),
        (
            ModelAction::Delete {
                model: model.into(),
            },
            "delete",
            "Delete",
        ),
        (
            ModelAction::Load {
                model: model.into(),
                keep_alive: "1h".into(),
            },
            "generate",
            "Post",
        ),
        (
            ModelAction::Unload {
                model: model.into(),
            },
            "generate",
            "Post",
        ),
    ] {
        let plan = plan("ollama", AdapterRequest::Action(action));
        assert_eq!(
            plan.requests[0].url,
            format!("http://inference.example:11434/api/{path}")
        );
        assert_eq!(format!("{:?}", plan.requests[0].method), method);
        assert_eq!(plan.requests[0].body.as_ref().unwrap()["model"], model);
    }
    let adapter = registry()
        .adapter("openai-compatible", "http://inference.example/v1")
        .unwrap();
    assert!(
        adapter
            .prepare(&mut session("openai-compatible"), AdapterRequest::State)
            .unwrap_err()
            .to_string()
            .contains("does not support")
    );
    assert!(
        registry()
            .adapter("missing", "http://inference.example")
            .err()
            .unwrap()
            .to_string()
            .contains("not installed")
    );
}

#[test]
fn fragmented_sse_and_usage_pass_through_wasm() {
    let mut decoder = Decoder {
        session: session("openai-compatible"),
        buffer: Vec::new(),
    };
    assert!(
        decoder
            .push(br#"data: {"choices":[{"delta":{"cont"#, false)
            .unwrap()
            .is_none()
    );
    let update=decoder.push(b"ent\":\"hello\"}}]}\r\n\ndata: {\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":4}}\n\ndata: [DONE]\n",false).unwrap().unwrap();
    assert_eq!(update.text, "hello");
    assert!(update.done);
    assert_eq!(update.prompt_tokens, Some(21));
    assert_eq!(update.completion_tokens, Some(4));
}
#[test]
fn fragmented_ndjson_utf8_and_eof_pass_through_wasm() {
    let mut decoder = Decoder {
        session: session("ollama"),
        buffer: Vec::new(),
    };
    let bytes="{\"message\":{\"content\":\"héllo\"},\"done\":false}\n{\"message\":{\"content\":\"!\"},\"done\":true,\"eval_count\":3}".as_bytes();
    let split = bytes.iter().position(|b| *b == 0xc3).unwrap() + 1;
    assert!(decoder.push(&bytes[..split], false).unwrap().is_none());
    let update = decoder.push(&bytes[split..], false).unwrap().unwrap();
    assert_eq!(update.text, "héllo");
    assert!(!update.done);
    let update = decoder.push(&[], true).unwrap().unwrap();
    assert_eq!(update.text, "!");
    assert!(update.done);
    assert_eq!(update.completion_tokens, Some(3));
}
#[test]
fn reasoning_effort_maps_to_provider_options() {
    let body = |id, options: &GenerationOptions| {
        let mut chat = request();
        chat.options = options.clone();
        plan(id, AdapterRequest::Stream(chat)).requests[0]
            .body
            .clone()
            .unwrap()
    };
    let mut options = request().options;
    options.reasoning_effort = Some("low".into());
    assert_eq!(body("ollama", &options)["think"], "low");
    assert_eq!(
        body("openai-compatible", &options)["reasoning_effort"],
        "low"
    );
    options.disable_reasoning = true;
    assert_eq!(body("ollama", &options)["think"], false);
    assert_eq!(
        body("openai-compatible", &options)["reasoning_effort"],
        "none"
    );
    let options = GenerationOptions::default();
    assert!(body("ollama", &options).get("think").is_none());
    assert!(
        body("openai-compatible", &options)
            .get("reasoning_effort")
            .is_none()
    );
}
#[test]
fn stream_updates_flag_hidden_reasoning_without_exposing_it() {
    for (id, thinking, answer) in [
        (
            "ollama",
            b"{\"message\":{\"content\":\"\",\"thinking\":\"hmm\"},\"done\":false}\n".as_slice(),
            b"{\"message\":{\"content\":\"Hi\"},\"done\":false}\n".as_slice(),
        ),
        (
            "openai-compatible",
            b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n".as_slice(),
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n".as_slice(),
        ),
    ] {
        let mut decoder = Decoder {
            session: session(id),
            buffer: Vec::new(),
        };
        let update = decoder.push(thinking, false).unwrap().unwrap();
        assert!(update.reasoning, "{id}");
        assert!(update.text.is_empty(), "{id}");
        let update = decoder.push(answer, false).unwrap().unwrap();
        assert!(!update.reasoning, "{id}");
        assert_eq!(update.text, "Hi", "{id}");
    }
}
#[test]
fn streaming_provider_errors_remain_errors() {
    for (id, bytes) in [
        (
            "openai-compatible",
            b"data: {\"error\":{\"message\":\"context window exceeded\"}}\n".as_slice(),
        ),
        (
            "ollama",
            b"{\"error\":\"context window exceeded\"}\n".as_slice(),
        ),
    ] {
        let mut decoder = Decoder {
            session: session(id),
            buffer: Vec::new(),
        };
        assert!(
            decoder
                .push(bytes, false)
                .unwrap_err()
                .to_string()
                .contains("context window exceeded")
        );
    }
}
#[test]
fn provider_responses_normalize_content_usage_and_finish_reason() {
    for (id, body) in [
        (
            "openai-compatible",
            json!({"choices":[{"message":{"content":"happy"},"finish_reason":"length"}],"usage":{"prompt_tokens":12,"completion_tokens":8}}),
        ),
        (
            "ollama",
            json!({"message":{"content":"happy"},"done_reason":"length","prompt_eval_count":12,"eval_count":8}),
        ),
    ] {
        let output = session(id)
            .invoke(&PluginInput::Decode {
                kind: ResponseKind::Completion,
                responses: vec![body],
            })
            .unwrap();
        let PluginOutput::Completion(value) = output else {
            panic!("expected completion");
        };
        assert_eq!(value.content.as_deref(), Some("happy"));
        assert_eq!(value.finish_reason.as_deref(), Some("length"));
        assert_eq!(
            value.usage,
            TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 8
            }
        );
    }
}

fn test_module(version: u32, body: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 1)
        (func (export "adapter_abi_version") (result i32) i32.const {version})
        (func (export "adapter_alloc") (param i32) (result i32) i32.const 4096)
        (func (export "adapter_free") (param i32 i32))
        (func (export "adapter_call") (param i32 i32) (result i64) {body})
    )"#
    ))
    .unwrap()
}
#[test]
fn incompatible_imported_and_oversized_plugins_are_rejected() {
    assert!(
        Plugin::load(&test_module(2, "i64.const 0"))
            .err()
            .unwrap()
            .to_string()
            .contains("ABI")
    );
    let imported =
        wat::parse_str("(module (import \"wasi_snapshot_preview1\" \"fd_write\" (func)))").unwrap();
    assert!(
        Plugin::load(&imported)
            .err()
            .unwrap()
            .to_string()
            .contains("must not import")
    );
    assert!(
        Plugin::load(&vec![0; wasm::MAX_WASM_BYTES + 1])
            .err()
            .unwrap()
            .to_string()
            .contains("exceeds")
    );
}
#[test]
fn looping_plugins_exhaust_fuel_and_oversized_output_is_rejected() {
    let plugin = Plugin::load(&test_module(1, "(loop br 0) i64.const 0")).unwrap();
    assert!(
        plugin
            .session()
            .unwrap()
            .invoke(&PluginInput::Stream {
                records: String::new()
            })
            .unwrap_err()
            .to_string()
            .contains("fuel")
    );
    let plugin = Plugin::load(&test_module(
        1,
        &format!("i64.const {}", MAX_MESSAGE_BYTES + 1),
    ))
    .unwrap();
    assert!(
        plugin
            .session()
            .unwrap()
            .invoke(&PluginInput::Stream {
                records: String::new()
            })
            .unwrap_err()
            .to_string()
            .contains("output exceeds")
    );
}
#[test]
fn out_of_bounds_guest_pointers_and_memory_are_rejected() {
    let pointer = (u64::from(u32::MAX) << 32) | 1;
    let plugin = Plugin::load(&test_module(1, &format!("i64.const {}", pointer as i64))).unwrap();
    assert!(
        plugin
            .session()
            .unwrap()
            .invoke(&PluginInput::Stream {
                records: String::new()
            })
            .is_err()
    );
    let bytes = wat::parse_str("(module (memory (export \"memory\") 1025))").unwrap();
    assert!(Plugin::load(&bytes).is_err());
}
#[test]
fn plugins_cannot_redirect_requests_to_another_origin() {
    let base = reqwest::Url::parse("http://inference.example:11434/v1").unwrap();
    let plan = HttpPlan::new(
        vec![HttpRequest::get(
            "http://other.example/api/models".into(),
            1000,
        )],
        ResponseKind::Models,
    );
    assert!(
        validate_plan(&base, &plan)
            .unwrap_err()
            .to_string()
            .contains("configured origin")
    );
}

struct PluginDirectory(std::path::PathBuf);
impl PluginDirectory {
    fn new(id: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "chatty-adapters-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let plugin = path.join("plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("adapter.json"),
            json!({"id":id,"api_version":1,"module":"adapter.wasm"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            plugin.join("adapter.wasm"),
            include_bytes!(concat!(
                env!("OUT_DIR"),
                "/wasm/wasm32-unknown-unknown/release/chatty_adapter_openai_compatible.wasm"
            )),
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for PluginDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn read_request(
    socket: tokio::net::TcpStream,
) -> (tokio::io::BufReader<tokio::net::TcpStream>, String, Value) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut reader = tokio::io::BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let first = line.clone();
    let mut length = 0;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (reader, first, body)
}
#[tokio::test]
async fn external_wasm_plugin_loads_and_calls_a_remote_server() {
    use tokio::io::AsyncWriteExt;
    let directory = PluginDirectory::new("custom-provider");
    let registry = Registry::load(Some(&directory.0)).unwrap();
    assert!(
        registry
            .inference_plugin_ids()
            .contains(&"custom-provider".into())
    );
    assert!(
        !registry
            .inference_plugin_ids()
            .contains(&"hugging-face".into())
    );
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", server.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (socket, _) = server.accept().await.unwrap();
        let (mut socket, first, _) = read_request(socket).await;
        assert_eq!(first, "GET /v1/models HTTP/1.1\r\n");
        let body = r#"{"data":[{"id":"served-model"}]}"#;
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
    });
    let models = registry
        .adapter("custom-provider", &url)
        .unwrap()
        .models(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(models, ["served-model"]);
    task.await.unwrap();
}
#[test]
fn external_manifests_reject_path_traversal_and_duplicate_builtin_ids() {
    let directory = PluginDirectory::new("custom-provider");
    std::fs::write(
        directory.0.join("plugin/adapter.json"),
        r#"{"id":"custom-provider","api_version":1,"module":"../adapter.wasm"}"#,
    )
    .unwrap();
    assert!(
        Registry::load(Some(&directory.0))
            .err()
            .unwrap()
            .to_string()
            .contains("filename")
    );
    let duplicate = PluginDirectory::new("openai-compatible");
    assert!(
        Registry::load(Some(&duplicate.0))
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate")
    );
}
#[tokio::test]
async fn stream_survives_cancelled_reads_and_drop_closes_upstream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for id in ["openai-compatible", "freetoken"] {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", server.local_addr().unwrap());
        let (resume, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (socket, _) = server.accept().await.unwrap();
            let (mut socket, first, body) = read_request(socket).await;
            assert_eq!(first, "POST /v1/chat/completions HTTP/1.1\r\n");
            assert_eq!(body["model"], "dynamic-model-id");
            socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"cont").await.unwrap();
            wait.await.unwrap();
            socket
                .get_mut()
                .write_all(b"ent\":\"hello\"}}]}\n\n")
                .await
                .unwrap();
            let mut byte = [0];
            let closed = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
                .await
                .unwrap();
            assert!(closed.is_err() || closed.unwrap() == 0);
        });
        let mut stream = registry()
            .adapter(id, &url)
            .unwrap()
            .stream(request())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), stream.next())
                .await
                .is_err()
        );
        resume.send(()).unwrap();
        let update = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(update.text, "hello");
        drop(stream);
        task.await.unwrap();
    }
}
#[tokio::test]
async fn redirects_and_non_streaming_mime_types_are_rejected() {
    use tokio::io::AsyncWriteExt;
    for (status, headers, needle) in [
        (
            "302 Found",
            "Location: http://another-server.example/models\r\n",
            "redirect",
        ),
        (
            "200 OK",
            "Content-Type: application/json\r\n",
            "does not support streaming",
        ),
    ] {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", server.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (socket, _) = server.accept().await.unwrap();
            let (mut socket, _, _) = read_request(socket).await;
            socket.get_mut().write_all(format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let error = registry()
            .adapter("openai-compatible", &url)
            .unwrap()
            .stream(request())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains(needle), "{error}");
        task.await.unwrap();
    }
}

fn fixed_reply_module(output: PluginOutput) -> Vec<u8> {
    let bytes = serde_json::to_vec(&PluginResult::Ok(output)).unwrap();
    let data = bytes
        .iter()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let text = format!(
        r#"(module
        (memory (export "memory") 1)
        (data (i32.const 0) "{data}")
        (func (export "adapter_abi_version") (result i32) i32.const 1)
        (func (export "adapter_alloc") (param i32) (result i32) i32.const 4096)
        (func (export "adapter_free") (param i32 i32))
        (func (export "adapter_call") (param i32 i32) (result i64) i64.const {})
    )"#,
        bytes.len()
    );
    wat::parse_str(text).unwrap()
}
#[test]
fn guest_plans_cannot_escalate_methods_or_extend_caller_timeouts() {
    let mut adapter = registry()
        .adapter("openai-compatible", "http://inference.example/v1")
        .unwrap();
    let mut request = HttpRequest::get("http://inference.example/v1/models".into(), 60_000);
    request.method = HttpMethod::Delete;
    let invalid = HttpPlan::new(vec![request], ResponseKind::Models);
    adapter.plugin = Plugin::load(&fixed_reply_module(PluginOutput::Plan(invalid))).unwrap();
    let error = adapter
        .prepare(
            &mut adapter.plugin.session().unwrap(),
            AdapterRequest::Models { timeout_ms: 2_000 },
        )
        .unwrap_err();
    assert!(error.to_string().contains("method is not allowed"));
    let valid = HttpPlan::new(
        vec![HttpRequest::get(
            "http://inference.example/v1/models".into(),
            60_000,
        )],
        ResponseKind::Models,
    );
    adapter.plugin = Plugin::load(&fixed_reply_module(PluginOutput::Plan(valid))).unwrap();
    let plan = adapter
        .prepare(
            &mut adapter.plugin.session().unwrap(),
            AdapterRequest::Models { timeout_ms: 2_000 },
        )
        .unwrap();
    assert_eq!(plan.requests[0].timeout_ms, Some(2_000));
}

#[test]
fn discovery_and_selection_do_not_load_unrequested_modules() {
    let directory = PluginDirectory::new("unused-provider");
    std::fs::write(
        directory.0.join("plugin/adapter.wasm"),
        b"not a wasm module",
    )
    .unwrap();
    let registry = Registry::load(Some(&directory.0)).unwrap();
    assert!(registry.selected.lock().unwrap().module.is_none());
    registry.select(Some("ollama")).unwrap();
    assert!(registry.selected.lock().unwrap().module.is_none());
    let selected = registry
        .selected_adapter("ollama", "http://inference.example:11434")
        .unwrap();
    assert_eq!(
        registry.selected.lock().unwrap().id.as_deref(),
        Some("ollama")
    );
    assert!(registry.selected.lock().unwrap().module.is_some());
    // A broken unused file neither blocks discovery nor loads while another provider runs.
    assert!(
        registry
            .adapter("unused-provider", "http://inference.example")
            .is_err()
    );
    assert!(
        registry
            .selected
            .lock()
            .unwrap()
            .module
            .as_ref()
            .unwrap()
            .weak_module()
            .ptr_eq(&selected.plugin.weak_module())
    );
}

#[test]
fn selected_module_is_reused_and_catalogue_is_released_after_use() {
    let registry = Registry::bundled().unwrap();
    registry.select(Some("ollama")).unwrap();
    let first = registry
        .selected_adapter("ollama", "http://inference.example:11434")
        .unwrap();
    let first_module = first.plugin.weak_module();
    let second = registry
        .selected_adapter("ollama", "http://inference.example:11434")
        .unwrap();
    assert!(first_module.ptr_eq(&second.plugin.weak_module()));
    let catalogue = registry
        .adapter("hugging-face", "https://huggingface.co")
        .unwrap();
    let catalogue_module = catalogue.plugin.weak_module();
    catalogue
        .prepare(
            &mut catalogue.plugin.session().unwrap(),
            AdapterRequest::Search {
                query: "llama".into(),
            },
        )
        .unwrap();
    assert!(catalogue_module.upgrade().is_some());
    drop(catalogue);
    assert!(catalogue_module.upgrade().is_none());
    assert_eq!(
        registry.selected.lock().unwrap().id.as_deref(),
        Some("ollama")
    );
    drop(first);
    drop(second);
    assert!(first_module.upgrade().is_some());
    registry.select(None).unwrap();
    assert!(first_module.upgrade().is_none());
    registry.select(Some("hugging-face")).unwrap();
    let catalogue = registry
        .selected_adapter("hugging-face", "https://huggingface.co")
        .unwrap();
    let module = catalogue.plugin.weak_module();
    drop(catalogue);
    assert!(module.upgrade().is_none());
    assert!(registry.selected.lock().unwrap().module.is_none());
}

#[test]
fn changing_selection_evicts_old_modules_without_breaking_active_requests() {
    let registry = Registry::bundled().unwrap();
    registry.select(Some("ollama")).unwrap();
    let active = registry
        .selected_adapter("ollama", "http://inference.example:11434")
        .unwrap();
    let old = active.plugin.weak_module();
    registry.select(Some("freetoken")).unwrap();
    assert!(registry.selected.lock().unwrap().module.is_none());
    assert!(old.upgrade().is_some());
    // An active request retains a valid module after its cache entry is evicted.
    active
        .prepare(
            &mut active.plugin.session().unwrap(),
            AdapterRequest::Models { timeout_ms: 1000 },
        )
        .unwrap();
    drop(active);
    assert!(old.upgrade().is_none());
    let current = registry
        .selected_adapter("freetoken", "http://inference.example:1919/v1")
        .unwrap();
    let current_module = current.plugin.weak_module();
    // A request carrying an old configuration cannot restore that provider to the cache.
    let stale = registry
        .selected_adapter("ollama", "http://inference.example:11434")
        .unwrap();
    let stale_module = stale.plugin.weak_module();
    drop(stale);
    assert!(stale_module.upgrade().is_none());
    assert!(
        registry
            .selected
            .lock()
            .unwrap()
            .module
            .as_ref()
            .unwrap()
            .weak_module()
            .ptr_eq(&current_module)
    );
    drop(current);
    registry.select(None).unwrap();
    assert!(current_module.upgrade().is_none());
}

#[test]
fn concurrent_selected_requests_share_one_compiled_module() {
    let registry = Registry::bundled().unwrap();
    registry.select(Some("ollama")).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let tasks = (0..4)
        .map(|_| {
            let registry = registry.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry
                    .selected_adapter("ollama", "http://inference.example:11434")
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let adapters = tasks
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>();
    let module = adapters[0].plugin.weak_module();
    assert!(
        adapters
            .iter()
            .all(|adapter| module.ptr_eq(&adapter.plugin.weak_module()))
    );
}

#[test]
fn management_and_inference_use_the_same_administrator_selection() {
    let registry = Registry::bundled().unwrap();
    let host = super::super::AdapterHost {
        registry: registry.clone(),
        selected_plugin: None,
        freetoken_credential: None,
        model_operations: Default::default(),
        workloads: Default::default(),
    };
    let mut config = crate::BrokerConfig {
        adapter_enabled: true,
        adapter_url: "http://inference.example:11434/v1".into(),
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
        reasoning: Default::default(),
    };
    host.configuration_changed(&config).unwrap();
    let compatible = host.inference(&config).unwrap();
    let selected = compatible.plugin.weak_module();
    let management = host.management(&config).unwrap();
    assert!(selected.ptr_eq(&management.plugin.weak_module()));
    assert!(
        management
            .prepare(
                &mut management.plugin.session().unwrap(),
                AdapterRequest::State
            )
            .unwrap_err()
            .to_string()
            .contains("does not support")
    );
    drop(compatible);
    drop(management);
    config.adapter_plugin = "ollama".into();
    host.configuration_changed(&config).unwrap();
    assert!(selected.upgrade().is_none());
    let management = host.management(&config).unwrap();
    let plan = management
        .prepare(
            &mut management.plugin.session().unwrap(),
            AdapterRequest::State,
        )
        .unwrap();
    assert_eq!(plan.requests.len(), 3);
    drop(management);
    config.adapter_plugin = "freetoken".into();
    host.configuration_changed(&config).unwrap();
    let inference = host.inference(&config).unwrap();
    let management = host.management(&config).unwrap();
    assert!(
        inference
            .plugin
            .weak_module()
            .ptr_eq(&management.plugin.weak_module())
    );
    assert_eq!(host.plugin_id(&config), "freetoken");
    let mut overridden = host.clone();
    overridden.selected_plugin = Some("openai-compatible".into());
    assert_eq!(overridden.plugin_id(&config), "openai-compatible");
    assert!(
        matches!(overridden.config_response(config.clone()), crate::Response::BrokerConfig {
        adapter_override: Some(id), ..
    } if id == "openai-compatible")
    );
    config.adapter_enabled = false;
    host.configuration_changed(&config).unwrap();
    assert!(registry.selected.lock().unwrap().module.is_none());
    assert!(host.inference(&config).is_err());
}
#[test]
fn image_plugins_are_separate_from_inference_plugins() {
    let registry = registry();
    assert_eq!(registry.image_plugin_ids(), ["comfyui"]);
    assert!(!registry.inference_plugin_ids().contains(&"comfyui".into()));
    assert!(registry.is_image("comfyui") && !registry.is_inference("comfyui"));
}

#[tokio::test]
async fn dropped_image_job_is_cancelled_upstream() {
    use tokio::io::AsyncWriteExt;
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (socket, _) = server.accept().await.unwrap();
            let (mut socket, first, body) = read_request(socket).await;
            let response = if first.starts_with("POST /prompt ") {
                r#"{"prompt_id":"job-7","number":1,"node_errors":{}}"#
            } else {
                "{}"
            };
            seen_tx.send((first, body)).unwrap();
            socket
                .get_mut()
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let adapter = registry().adapter("comfyui", &url).unwrap();
    let job = adapter.generate_image(ImageRequest {
        model: "model.safetensors".into(),
        prompt: "harbour".into(),
        negative_prompt: String::new(),
        width: 512,
        height: 512,
        steps: 4,
        guidance: 1.0,
        seed: 1,
        timeout_ms: 60_000,
        workflow: None,
    });
    // The job stays pending; the timeout drops it during its first poll wait.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), job)
            .await
            .is_err()
    );
    let mut requests = Vec::new();
    while requests.len() < 3 {
        requests.push(
            tokio::time::timeout(Duration::from_secs(5), seen_rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    requests.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(requests[0].0.starts_with("POST /interrupt "));
    assert_eq!(requests[0].1["prompt_id"], "job-7");
    assert!(requests[1].0.starts_with("POST /prompt "));
    assert!(requests[2].0.starts_with("POST /queue "));
    assert_eq!(requests[2].1["delete"][0], "job-7");
}

#[tokio::test]
#[ignore = "requires CHATTY_TEST_IMAGE_URL; optional CHATTY_TEST_IMAGE_MODEL generates one image, CHATTY_TEST_IMAGE_WORKFLOW selects a template file, CHATTY_TEST_IMAGE_OUT saves it"]
async fn live_comfyui_lists_models_and_generates() {
    let url = std::env::var("CHATTY_TEST_IMAGE_URL").unwrap();
    let adapter = registry().adapter("comfyui", &url).unwrap();
    let models = adapter.models(Duration::from_secs(10)).await.unwrap();
    eprintln!("checkpoints: {models:?}");
    let model =
        std::env::var("CHATTY_TEST_IMAGE_MODEL").unwrap_or_else(|_| "missing.safetensors".into());
    let result = adapter
        .generate_image(ImageRequest {
            model,
            prompt: "a red lighthouse on a cliff at dusk".into(),
            negative_prompt: String::new(),
            width: 512,
            height: 512,
            steps: 8,
            guidance: 5.0,
            seed: 42,
            timeout_ms: 600_000,
            workflow: std::env::var("CHATTY_TEST_IMAGE_WORKFLOW")
                .ok()
                .map(|path| std::fs::read_to_string(path).unwrap()),
        })
        .await;
    match result {
        Ok(images) => {
            if let Ok(path) = std::env::var("CHATTY_TEST_IMAGE_OUT") {
                std::fs::write(path, &images[0]).unwrap();
            }
            eprintln!(
                "generated {} image(s), first {} bytes",
                images.len(),
                images[0].len()
            );
        }
        Err(error) => eprintln!("generation error: {error}"),
    }
}
