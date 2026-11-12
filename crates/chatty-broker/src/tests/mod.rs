//! Broker-level tests that drive `dispatch` end to end.
use super::*;
use crate::images::generate_png;

mod account;
mod admin;
mod characters;
mod conversations;
mod downloads;
mod images;
mod infra;
mod memory;
mod tenancy;

pub(crate) fn test_portrait_png() -> Vec<u8> {
    let pixels = vec![80u8; 128 * 128 * 4];
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        &pixels,
        128,
        128,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    png
}

pub(crate) async fn call(app: &App, request: Request) -> Result<(MessageType, Bytes)> {
    let (tx, mut rx) = mpsc::channel(32);
    let dispatch_app = app.clone();
    let task = tokio::spawn(async move {
        dispatch(
            dispatch_app,
            tx,
            Arc::new(RwLock::new(None)),
            new_uuid(),
            1,
            request,
            Arc::new(std::sync::atomic::AtomicU64::new(0)).into(),
        )
        .await
    });
    let mut result = None;
    while let Some((kind, _, payload)) = rx.recv().await {
        if matches!(kind, MessageType::Response | MessageType::Error) {
            result = Some((kind, payload));
            break;
        }
    }
    task.await??;
    result.context("missing response")
}

/// Minimal ComfyUI HTTP surface: one queued job which is ready on its second poll.
/// It also answers the native Ollama calls used for shared-host model switching.
pub(crate) async fn mock_comfyui(
    png: Vec<u8>,
) -> (String, Arc<std::sync::Mutex<Vec<(String, Value)>>>) {
    mock_comfyui_with_ollama(png, true).await
}

/// `ollama_unloads` false simulates a chat model that never leaves the GPU.
pub(crate) async fn mock_comfyui_with_ollama(
    png: Vec<u8>,
    ollama_unloads: bool,
) -> (String, Arc<std::sync::Mutex<Vec<(String, Value)>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let server = crate::test_net::mock_listener().await;
    let url = format!("http://{}", server.local_addr().unwrap());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let mut polls = 0;
        let mut text_loaded = true;
        loop {
            let (socket, _) = server.accept().await.unwrap();
            let mut socket = tokio::io::BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            let request = line
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
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
            let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if ollama_unloads && request == "POST /api/generate" && body["keep_alive"] == 0 {
                text_loaded = false;
            }
            if request == "POST /api/chat" {
                text_loaded = true;
            }
            log.lock().unwrap().push((request.clone(), body));
            let (content_type, response) = if request == "GET /models/checkpoints" {
                ("application/json", br#"["model.safetensors"]"#.to_vec())
            } else if request == "GET /models/diffusion_models" {
                ("application/json", br#"["flux1-dev.safetensors"]"#.to_vec())
            } else if request == "POST /prompt" {
                (
                    "application/json",
                    br#"{"prompt_id":"job-1","number":1,"node_errors":{}}"#.to_vec(),
                )
            } else if request == "GET /history/job-1" {
                polls += 1;
                let body = if polls % 2 == 1 {
                    "{}"
                } else {
                    r#"{"job-1":{"status":{"status_str":"success","completed":true},"outputs":{"7":{"images":[{"filename":"a.png","subfolder":"","type":"temp"}]}}}}"#
                };
                ("application/json", body.as_bytes().to_vec())
            } else if request.starts_with("GET /view?") {
                ("image/png", png.clone())
            } else if request == "GET /api/version" {
                ("application/json", br#"{"version":"0.12.0"}"#.to_vec())
            } else if request == "GET /api/tags" {
                (
                    "application/json",
                    br#"{"models":[{"name":"chat-model"}]}"#.to_vec(),
                )
            } else if request == "GET /api/ps" {
                let running: &[u8] = if text_loaded {
                    br#"{"models":[{"name":"chat-model","size":1,"size_vram":1,"expires_at":""}]}"#
                } else {
                    br#"{"models":[]}"#
                };
                ("application/json", running.to_vec())
            } else if request == "POST /api/chat" {
                (
                    "application/json",
                    br#"{"message":{"content":"ok"},"done_reason":"stop"}"#.to_vec(),
                )
            } else {
                ("application/json", b"{}".to_vec())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            let socket = socket.get_mut();
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&response).await.unwrap();
        }
    });
    (url, seen)
}
