//! Private wire contract between the broker and the `chatty-metal` appliance.
//!
//! This is not a public provider API. The broker is the only client. The framing
//! matches the client protocol: payload length (u32 BE), flags (u8), type (u8),
//! request id (u64 BE), then a bincode payload that may be zstd-compressed.
//!
//! The appliance is a single-model, single-worker server. One connection carries
//! one handshake and then one request. Dropping the connection cancels the work.

#![deny(unsafe_code)]

use serde::{Serialize, de::DeserializeOwned};
use std::{
    fmt, io,
    io::{Cursor, Read, Write},
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 14;
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
pub const COMPRESSION_THRESHOLD: usize = 256;
pub const FLAG_ZSTD: u8 = 1;

pub const MAX_CLIENT_BYTES: usize = 64;
pub const MAX_MESSAGE_BYTES: usize = 512 * 1024;
pub const MAX_PROMPT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PROMPT_PIECES: usize = 1024;
pub const MAX_OUTPUT_TOKENS: u32 = 32 * 1024;

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    Codec(String),
    Invalid(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Codec(error) => write!(f, "codec: {error}"),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Hello = 1,
    HelloAck = 2,
    StatusRequest = 3,
    Status = 4,
    Generate = 5,
    Chunk = 6,
    Done = 7,
    Cancel = 8,
    Error = 9,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::HelloAck,
            3 => Self::StatusRequest,
            4 => Self::Status,
            5 => Self::Generate,
            6 => Self::Chunk,
            7 => Self::Done,
            8 => Self::Cancel,
            9 => Self::Error,
            _ => return Err(ProtocolError::Invalid("unknown message type".into())),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum ServerState {
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum FinishReason {
    EndOfSentence,
    Length,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum ErrorCode {
    Protocol,
    Unsupported,
    NotReady,
    InvalidRequest,
    ContextExceeded,
    Internal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Hello {
    pub version: u16,
    pub client: String,
}

impl Hello {
    pub fn new(client: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            client: client.into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::Invalid(format!(
                "unsupported protocol version {}",
                self.version
            )));
        }
        if self.client.len() > MAX_CLIENT_BYTES {
            return Err(ProtocolError::Invalid("client name is too long".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct HelloAck {
    pub version: u16,
    pub model: String,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub state: ServerState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Status {
    pub state: ServerState,
    pub detail: String,
    pub model: String,
    pub context_tokens: u32,
    pub uptime_ms: u64,
    pub tokens_generated: u64,
    pub requests_served: u64,
    pub last_decode_tokens_per_second: f32,
    pub queue_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum Marker {
    BeginOfSentence,
    User,
    Assistant,
    EndOfSentence,
    ThoughtEnd,
}

/// A trusted marker or plain text. Text is encoded with special tokens treated
/// as ordinary bytes, so user content cannot forge a role boundary.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub enum Piece {
    Marker(Marker),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
pub struct Sampling {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub seed: u64,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            temperature: 0.8,
            top_p: 0.95,
            top_k: 40,
            seed: 0,
        }
    }
}

impl Sampling {
    pub fn validate(&self) -> Result<()> {
        if !self.temperature.is_finite() || !(0.0..=4.0).contains(&self.temperature) {
            return Err(ProtocolError::Invalid(
                "temperature must be finite and between 0 and 4".into(),
            ));
        }
        if !self.top_p.is_finite() || !(0.0..=1.0).contains(&self.top_p) {
            return Err(ProtocolError::Invalid(
                "top_p must be finite and between 0 and 1".into(),
            ));
        }
        if self.top_k > 1024 {
            return Err(ProtocolError::Invalid("top_k is too large".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Generate {
    pub prompt: Vec<Piece>,
    pub sampling: Sampling,
    pub max_tokens: u32,
}

impl Generate {
    pub fn validate(&self) -> Result<()> {
        if self.prompt.is_empty() {
            return Err(ProtocolError::Invalid("prompt is empty".into()));
        }
        if self.prompt.len() > MAX_PROMPT_PIECES {
            return Err(ProtocolError::Invalid("prompt has too many pieces".into()));
        }
        let mut bytes = 0usize;
        for piece in &self.prompt {
            if let Piece::Text(text) = piece {
                bytes = bytes.saturating_add(text.len());
            }
        }
        if bytes > MAX_PROMPT_BYTES {
            return Err(ProtocolError::Invalid("prompt is too large".into()));
        }
        if self.max_tokens == 0 || self.max_tokens > MAX_OUTPUT_TOKENS {
            return Err(ProtocolError::Invalid(
                "max_tokens is outside the supported range".into(),
            ));
        }
        self.sampling.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Chunk {
    pub sequence: u32,
    pub text: String,
    #[serde(default)]
    pub reasoning: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Done {
    pub finish: FinishReason,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub decode_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Cancel {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub message_type: MessageType,
    pub request_id: u64,
    pub compressed: bool,
    pub payload: Vec<u8>,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|error| ProtocolError::Codec(error.to_string()))
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let (value, used) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map_err(|error| ProtocolError::Codec(error.to_string()))?;
    if used != bytes.len() {
        return Err(ProtocolError::Invalid("payload has trailing bytes".into()));
    }
    Ok(value)
}

pub fn write_message<W: Write, T: Serialize>(
    writer: &mut W,
    message_type: MessageType,
    request_id: u64,
    value: &T,
) -> Result<()> {
    write_payload(writer, message_type, request_id, &encode(value)?)
}

pub fn write_payload<W: Write>(
    writer: &mut W,
    message_type: MessageType,
    request_id: u64,
    payload: &[u8],
) -> Result<()> {
    let (flags, payload) = if payload.len() >= COMPRESSION_THRESHOLD {
        (FLAG_ZSTD, compress(payload)?)
    } else {
        (0, payload.to_vec())
    };
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload is too large".into()));
    }
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    header[4] = flags;
    header[5] = message_type as u8;
    header[6..].copy_from_slice(&request_id.to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let flags = header[4];
    let message_type = MessageType::try_from(header[5])?;
    let request_id = u64::from_be_bytes(header[6..].try_into().expect("eight byte request id"));
    if len > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload is too large".into()));
    }
    if flags & !FLAG_ZSTD != 0 {
        return Err(ProtocolError::Invalid("unknown frame flags".into()));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    let compressed = flags & FLAG_ZSTD != 0;
    if compressed {
        payload = decompress(&payload)?;
    }
    Ok(Frame {
        message_type,
        request_id,
        compressed,
        payload,
    })
}

fn compress(payload: &[u8]) -> Result<Vec<u8>> {
    zstd::stream::encode_all(Cursor::new(payload), 3).map_err(ProtocolError::Io)
}

fn decompress(payload: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    zstd::stream::read::Decoder::new(Cursor::new(payload))?
        .take((MAX_PAYLOAD + 1) as u64)
        .read_to_end(&mut decoded)?;
    if decoded.len() > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid(
            "decompressed payload is too large".into(),
        ));
    }
    Ok(decoded)
}

#[cfg(feature = "net")]
pub mod net {
    //! Async framing for the broker.

    use super::{
        Frame, HEADER_LEN, MAX_PAYLOAD, MessageType, ProtocolError, Result, decode, encode,
    };
    use serde::Serialize;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
        writer: &mut W,
        message_type: MessageType,
        request_id: u64,
        value: &T,
    ) -> Result<()> {
        write_payload(writer, message_type, request_id, &encode(value)?).await
    }

    pub async fn write_payload<W: AsyncWrite + Unpin>(
        writer: &mut W,
        message_type: MessageType,
        request_id: u64,
        payload: &[u8],
    ) -> Result<()> {
        let (flags, payload) = if payload.len() >= super::COMPRESSION_THRESHOLD {
            (super::FLAG_ZSTD, super::compress(payload)?)
        } else {
            (0, payload.to_vec())
        };
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::Invalid("payload is too large".into()));
        }
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        header[4] = flags;
        header[5] = message_type as u8;
        header[6..].copy_from_slice(&request_id.to_be_bytes());
        writer.write_all(&header).await?;
        writer.write_all(&payload).await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
        let mut header = [0u8; HEADER_LEN];
        reader.read_exact(&mut header).await?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let flags = header[4];
        let message_type = MessageType::try_from(header[5])?;
        let request_id = u64::from_be_bytes(header[6..].try_into().expect("eight byte request id"));
        if len > MAX_PAYLOAD {
            return Err(ProtocolError::Invalid("payload is too large".into()));
        }
        if flags & !super::FLAG_ZSTD != 0 {
            return Err(ProtocolError::Invalid("unknown frame flags".into()));
        }
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).await?;
        let compressed = flags & super::FLAG_ZSTD != 0;
        if compressed {
            payload = super::decompress(&payload)?;
        }
        Ok(Frame {
            message_type,
            request_id,
            compressed,
            payload,
        })
    }

    pub async fn read_message<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
        reader: &mut R,
    ) -> Result<(MessageType, u64, T)> {
        let frame = read_frame(reader).await?;
        let value = decode(&frame.payload)?;
        Ok((frame.message_type, frame.request_id, value))
    }
}

#[cfg(test)]
mod tests;
