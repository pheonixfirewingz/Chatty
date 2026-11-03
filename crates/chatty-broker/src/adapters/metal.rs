//! Native client for the private `chatty-metal` appliance protocol.
//!
//! The appliance is the only inference backend that does not speak HTTP. Prompt
//! assembly stays here: the broker sends trusted role markers plus text, and the
//! appliance only tokenizes and runs the model.

use super::runtime::{ChatMessage, ChatRequest, Completion, Error, GenerationOptions, Result};
use chatty_metal_protocol as wire;
use std::time::Duration;
use tokio::{net::TcpStream, time::Instant};

const DEFAULT_PORT: u16 = 11434;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const DEFAULT_MAX_TOKENS: u32 = 512;

#[derive(Clone, Debug)]
pub struct MetalClient {
    endpoint: String,
}

pub struct MetalStream {
    stream: TcpStream,
    request_id: u64,
    deadline: Instant,
    done: bool,
}

impl MetalClient {
    /// Accept `metal://host[:port]`. The scheme is deliberate: this is not HTTP,
    /// so an OpenAI-compatible URL cannot silently point at the appliance.
    pub fn new(url: &str) -> Result<Self> {
        let rest = url
            .strip_prefix("metal://")
            .ok_or_else(|| Error::new("chatty-metal URL must use metal://"))?;
        if rest.is_empty()
            || rest.contains(['/', '?', '#', '@'])
            || rest.chars().any(char::is_whitespace)
        {
            return Err(Error::new("chatty-metal URL is invalid"));
        }
        let endpoint = if rest
            .rsplit(':')
            .next()
            .is_some_and(|port| port.parse::<u16>().is_ok() && rest.matches(':').count() == 1)
        {
            rest.to_owned()
        } else {
            format!("{rest}:{DEFAULT_PORT}")
        };
        Ok(Self { endpoint })
    }

    async fn connect(&self) -> Result<(TcpStream, wire::HelloAck)> {
        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&self.endpoint))
            .await
            .map_err(|_| Error::new("chatty-metal connection timed out"))??;
        stream.set_nodelay(true)?;
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            wire::net::write_message(
                &mut stream,
                wire::MessageType::Hello,
                0,
                &wire::Hello::new("chatty-broker"),
            ),
        )
        .await
        .map_err(|_| Error::new("chatty-metal handshake timed out"))??;
        let frame = tokio::time::timeout(HANDSHAKE_TIMEOUT, wire::net::read_frame(&mut stream))
            .await
            .map_err(|_| Error::new("chatty-metal handshake timed out"))??;
        if frame.message_type != wire::MessageType::HelloAck {
            return Err(Error::new("chatty-metal did not acknowledge the handshake"));
        }
        let ack: wire::HelloAck = wire::decode(&frame.payload)?;
        if ack.version != wire::PROTOCOL_VERSION {
            return Err(Error::new(format!(
                "chatty-metal protocol version {} is not supported",
                ack.version
            )));
        }
        Ok((stream, ack))
    }

    pub async fn models(&self, timeout: Duration) -> Result<Vec<String>> {
        let (_, ack) = tokio::time::timeout(timeout, self.connect())
            .await
            .map_err(|_| Error::new("chatty-metal did not answer in time"))??;
        if ack.state != wire::ServerState::Ready {
            return Err(Error::new(format!(
                "chatty-metal is not ready: {}",
                ack.detail
            )));
        }
        if ack.model.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![ack.model])
    }

    pub async fn complete(&self, request: ChatRequest) -> Result<Completion> {
        let timeout = request
            .timeout_ms
            .filter(|millis| *millis > 0)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        tokio::time::timeout(timeout, self.complete_inner(request))
            .await
            .map_err(|_| Error::new("chatty-metal generation timed out"))?
    }

    async fn complete_inner(&self, request: ChatRequest) -> Result<Completion> {
        let (mut stream, ack) = self.connect().await?;
        if ack.state != wire::ServerState::Ready {
            return Err(Error::new(format!(
                "chatty-metal is not ready: {}",
                ack.detail
            )));
        }
        let generate = generate_request(&request);
        generate.validate()?;
        let request_id = 1;
        wire::net::write_message(
            &mut stream,
            wire::MessageType::Generate,
            request_id,
            &generate,
        )
        .await?;
        let mut text = String::new();
        loop {
            let frame = wire::net::read_frame(&mut stream).await?;
            if frame.request_id != request_id {
                return Err(Error::new("chatty-metal answered a different request"));
            }
            match frame.message_type {
                wire::MessageType::Chunk => {
                    let chunk: wire::Chunk = wire::decode(&frame.payload)?;
                    text.push_str(&chunk.text);
                }
                wire::MessageType::Done => {
                    let done: wire::Done = wire::decode(&frame.payload)?;
                    return Ok(Completion {
                        content: Some(text),
                        finish_reason: Some(finish_reason(done.finish).into()),
                        usage: crate::adapter_api::TokenUsage {
                            prompt_tokens: u64::from(done.prompt_tokens),
                            completion_tokens: u64::from(done.completion_tokens),
                        },
                    });
                }
                wire::MessageType::Error => {
                    let error: wire::WireError = wire::decode(&frame.payload)?;
                    return Err(Error::new(error.message));
                }
                _ => return Err(Error::new("chatty-metal sent an unexpected frame")),
            }
        }
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<MetalStream> {
        let timeout = request
            .timeout_ms
            .filter(|millis| *millis > 0)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        let (mut stream, ack) = tokio::time::timeout(timeout, self.connect())
            .await
            .map_err(|_| Error::new("chatty-metal generation timed out"))??;
        if ack.state != wire::ServerState::Ready {
            return Err(Error::new(format!(
                "chatty-metal is not ready: {}",
                ack.detail
            )));
        }
        let generate = generate_request(&request);
        generate.validate()?;
        let request_id = 1;
        wire::net::write_message(
            &mut stream,
            wire::MessageType::Generate,
            request_id,
            &generate,
        )
        .await?;
        Ok(MetalStream {
            stream,
            request_id,
            deadline: Instant::now() + timeout,
            done: false,
        })
    }

    pub async fn status(&self) -> Result<wire::Status> {
        let (mut stream, _) = self.connect().await?;
        wire::net::write_message(&mut stream, wire::MessageType::StatusRequest, 1, &()).await?;
        let frame = wire::net::read_frame(&mut stream).await?;
        if frame.message_type != wire::MessageType::Status {
            return Err(Error::new("chatty-metal did not return status"));
        }
        Ok(wire::decode(&frame.payload)?)
    }
}

impl MetalStream {
    pub async fn next(&mut self) -> Result<Option<crate::adapter_api::StreamUpdate>> {
        if self.done {
            return Ok(None);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::new("chatty-metal generation timed out"));
        }
        let frame = tokio::time::timeout_at(self.deadline, wire::net::read_frame(&mut self.stream))
            .await
            .map_err(|_| Error::new("chatty-metal generation timed out"))??;
        if frame.request_id != self.request_id {
            return Err(Error::new("chatty-metal answered a different request"));
        }
        match frame.message_type {
            wire::MessageType::Chunk => {
                let chunk: wire::Chunk = wire::decode(&frame.payload)?;
                Ok(Some(crate::adapter_api::StreamUpdate {
                    text: chunk.text,
                    ..Default::default()
                }))
            }
            wire::MessageType::Done => {
                let done: wire::Done = wire::decode(&frame.payload)?;
                self.done = true;
                Ok(Some(crate::adapter_api::StreamUpdate {
                    finish_reason: Some(finish_reason(done.finish).into()),
                    prompt_tokens: Some(u64::from(done.prompt_tokens)),
                    completion_tokens: Some(u64::from(done.completion_tokens)),
                    done: true,
                    ..Default::default()
                }))
            }
            wire::MessageType::Error => {
                let error: wire::WireError = wire::decode(&frame.payload)?;
                Err(Error::new(error.message))
            }
            _ => Err(Error::new("chatty-metal sent an unexpected frame")),
        }
    }
}

fn finish_reason(reason: wire::FinishReason) -> &'static str {
    match reason {
        wire::FinishReason::EndOfSentence => "stop",
        wire::FinishReason::Length => "length",
        wire::FinishReason::Cancelled => "cancelled",
    }
}

/// DeepSeek-V4 role markers. The model tokenizer treats plain text as bytes, so
/// user content cannot forge a marker.
fn prompt(messages: &[ChatMessage], options: &GenerationOptions) -> Vec<wire::Piece> {
    let mut pieces = vec![wire::Piece::Marker(wire::Marker::BeginOfSentence)];
    let system = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !system.is_empty() {
        pieces.push(wire::Piece::Text(format!("{system}\n\n")));
    }
    for message in messages {
        match message.role.as_str() {
            "user" => {
                pieces.push(wire::Piece::Marker(wire::Marker::User));
                pieces.push(wire::Piece::Text(message.content.clone()));
            }
            "assistant" => {
                pieces.push(wire::Piece::Marker(wire::Marker::Assistant));
                pieces.push(wire::Piece::Text(message.content.clone()));
                pieces.push(wire::Piece::Marker(wire::Marker::EndOfSentence));
            }
            _ => {}
        }
    }
    pieces.push(wire::Piece::Marker(wire::Marker::Assistant));
    if !options.disable_reasoning {
        pieces.push(wire::Piece::Text("</think>".into()));
    }
    pieces
}

fn generate_request(request: &ChatRequest) -> wire::Generate {
    let options = &request.options;
    wire::Generate {
        prompt: prompt(&request.messages, options),
        sampling: wire::Sampling {
            temperature: options.temperature.unwrap_or(0.8).clamp(0.0, 4.0),
            top_p: options.top_p.unwrap_or(0.95).clamp(0.0, 1.0),
            top_k: options.top_k.unwrap_or(40).min(1024),
            seed: options.seed.unwrap_or(0).max(0) as u64,
        },
        max_tokens: options
            .max_tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| (tokens as u32).min(wire::MAX_OUTPUT_TOKENS))
            .unwrap_or(DEFAULT_MAX_TOKENS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_metal_urls_and_defaults_the_port() {
        assert_eq!(
            MetalClient::new("metal://192.168.0.97").unwrap().endpoint,
            "192.168.0.97:11434"
        );
        assert_eq!(
            MetalClient::new("metal://192.168.0.97:11435")
                .unwrap()
                .endpoint,
            "192.168.0.97:11435"
        );
    }

    #[test]
    fn refuses_http_urls_and_paths() {
        assert!(MetalClient::new("http://192.168.0.97:11434").is_err());
        assert!(MetalClient::new("metal://192.168.0.97:11434/v1").is_err());
        assert!(MetalClient::new("metal://").is_err());
    }

    #[test]
    fn renders_system_user_and_assistant_turns_with_markers() {
        let messages = vec![
            ChatMessage::new("system", "rules"),
            ChatMessage::new("user", "hello"),
            ChatMessage::new("assistant", "hi"),
            ChatMessage::new("user", "again"),
        ];
        let pieces = prompt(&messages, &GenerationOptions::default());
        assert_eq!(
            pieces[0],
            wire::Piece::Marker(wire::Marker::BeginOfSentence)
        );
        assert_eq!(pieces[1], wire::Piece::Text("rules\n\n".into()));
        assert_eq!(pieces[2], wire::Piece::Marker(wire::Marker::User));
        assert_eq!(pieces[3], wire::Piece::Text("hello".into()));
        assert_eq!(pieces[4], wire::Piece::Marker(wire::Marker::Assistant));
        assert_eq!(pieces[5], wire::Piece::Text("hi".into()));
        assert_eq!(pieces[6], wire::Piece::Marker(wire::Marker::EndOfSentence));
        assert_eq!(pieces[7], wire::Piece::Marker(wire::Marker::User));
        assert_eq!(pieces[8], wire::Piece::Text("again".into()));
        assert_eq!(pieces[9], wire::Piece::Marker(wire::Marker::Assistant));
        assert_eq!(pieces[10], wire::Piece::Text("</think>".into()));
    }

    #[test]
    fn maps_generation_options_with_bounds() {
        let request = ChatRequest {
            model: "ignored".into(),
            messages: vec![ChatMessage::new("user", "hi")],
            options: GenerationOptions {
                temperature: Some(9.0),
                top_p: Some(-1.0),
                top_k: Some(9999),
                max_tokens: Some(-1),
                seed: Some(-5),
                ..GenerationOptions::default()
            },
            timeout_ms: None,
        };
        let generate = generate_request(&request);
        assert_eq!(generate.sampling.temperature, 4.0);
        assert_eq!(generate.sampling.top_p, 0.0);
        assert_eq!(generate.sampling.top_k, 1024);
        assert_eq!(generate.sampling.seed, 0);
        assert_eq!(generate.max_tokens, DEFAULT_MAX_TOKENS);
    }
}
