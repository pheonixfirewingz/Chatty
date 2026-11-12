//! Deterministic OpenAI-compatible test server for transport/streaming soaks.
//! This executable is test support, never embedded in the broker or client.

use chatty_protocol::current_utc_timestamp;
use chatty_protocol::util::args::ParsedArgs;
use chatty_protocol::util::{Context, Result, bail};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    time::{Duration, sleep},
};

struct Args {
    listen: String,
    words: usize,
    chunk_delay_ms: u64,
}

/// Scripted non-streaming replies, served in order; `None` keeps the fixed reply.
type Script = Option<Arc<Mutex<VecDeque<String>>>>;

const USAGE: &str = "chatty-mock-llama -- deterministic OpenAI-compatible test server\n\n\
Options:\n\
  --listen <addr>        Bind address [default: 127.0.0.1:18114]\n\
  --words <n>            Words per completion [default: 48]\n\
  --chunk-delay-ms <ms>  Delay between stream chunks [default: 5]\n\
  --memory-fixtures <dir> Answer non-streaming completions with each memory\n\
                         fixture's scripted output, in file-name order\n";

#[tokio::main]
async fn main() -> Result<()> {
    let parsed = ParsedArgs::parse(USAGE)?;
    let args = Args {
        listen: parsed.string("listen", "", "127.0.0.1:18114"),
        words: parsed.number("words", 48),
        chunk_delay_ms: parsed.number("chunk-delay-ms", 5),
    };
    let script = match parsed.optional("memory-fixtures", "") {
        Some(directory) => Some(Arc::new(Mutex::new(load_script(&directory)?))),
        None => None,
    };
    let listener = TcpListener::bind(&args.listen).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(handle(
            stream,
            args.words,
            args.chunk_delay_ms,
            script.clone(),
        ));
    }
}

/// Reads the `scripted` output of every `*.json` memory fixture in `directory`.
fn load_script(directory: &str) -> Result<VecDeque<String>> {
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
            fixture["scripted"]
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("{} has no scripted output", path.display()))
        })
        .collect()
}

async fn handle(mut stream: TcpStream, words: usize, delay_ms: u64, script: Script) {
    if let Err(error) = respond(&mut stream, words, delay_ms, script).await {
        eprintln!("{} · mock request failed: {error}", current_utc_timestamp());
    }
}

async fn respond(
    stream: &mut TcpStream,
    words: usize,
    delay_ms: u64,
    script: Script,
) -> Result<()> {
    let mut request = Vec::with_capacity(4096);
    let mut buffer = [0u8; 1024];
    let header_end = loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            bail!("request ended before headers")
        }
        request.extend_from_slice(&buffer[..count]);
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if request.len() > 64 * 1024 {
            bail!("request headers too large")
        }
    };
    let headers = std::str::from_utf8(&request[..header_end])?;
    let first_line = headers.lines().next().context("missing request line")?;
    let is_models = first_line.starts_with("GET /v1/models ");
    let is_completions = first_line.starts_with("POST /v1/chat/completions ");
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            bail!("request body truncated")
        }
        request.extend_from_slice(&buffer[..count]);
    }

    if is_models {
        return json_response(stream, json!({"data":[{"id":"chatty-test-model"}]})).await;
    }
    if !is_completions {
        return status(stream, "404 Not Found", "not found").await;
    }
    let body: serde_json::Value =
        serde_json::from_slice(&request[header_end..header_end + content_length])?;
    if body["stream"].as_bool() != Some(true) {
        let content = match &script {
            // An exhausted script answers that nothing is worth saving.
            Some(script) => script
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| "[]".into()),
            None => "A durable test memory.".into(),
        };
        return json_response(stream, json!({"choices":[{"message":{"content":content}}]})).await;
    }
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await?;
    for index in 0..words {
        let event = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":format!("word-{index} ")}}]})
        );
        stream.write_all(event.as_bytes()).await?;
        sleep(Duration::from_millis(delay_ms)).await;
    }
    stream.write_all(b"data: [DONE]\n\n").await?;
    stream.shutdown().await?;
    Ok(())
}

async fn json_response(stream: &mut TcpStream, value: serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(&value)?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn status(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}
