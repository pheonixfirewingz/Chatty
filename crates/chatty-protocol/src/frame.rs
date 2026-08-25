//! Wire framing: the fixed 14-byte header shared by every message.
//!
//! Layout: payload length (u32 BE), flags (u8), type (u8), request id (u64 BE).
//! Payloads are bincode-encoded; eligible payloads are zstd-compressed.

use bytes::{Buf, BufMut, BytesMut};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Cursor, Read};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::ProtocolError;

pub const HEADER_LEN: usize = 14;
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
pub const COMPRESSION_THRESHOLD: usize = 256;
pub const FLAG_ZSTD: u8 = 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Handshake = 1,
    Request = 2,
    Response = 3,
    Delta = 4,
    StreamChunk = 5,
    StreamEnd = 6,
    Error = 7,
    Cancel = 8,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        Ok(match value {
            1 => Self::Handshake,
            2 => Self::Request,
            3 => Self::Response,
            4 => Self::Delta,
            5 => Self::StreamChunk,
            6 => Self::StreamEnd,
            7 => Self::Error,
            8 => Self::Cancel,
            _ => return Err(ProtocolError::Invalid("unknown message type")),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub compressed: bool,
    pub message_type: MessageType,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|e| ProtocolError::Codec(e.to_string()))
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map(|x| x.0)
        .map_err(|e| ProtocolError::Codec(e.to_string()))
}

pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    ty: MessageType,
    request_id: u64,
    value: &T,
) -> Result<(), ProtocolError> {
    let raw = encode(value)?;
    write_payload(writer, ty, request_id, raw).await
}

/// Frames an already-bincode-encoded payload. Useful for bounded writer queues.
pub async fn write_payload<W: AsyncWrite + Unpin>(
    writer: &mut W,
    ty: MessageType,
    request_id: u64,
    raw: Vec<u8>,
) -> Result<(), ProtocolError> {
    let must_compress = matches!(ty, MessageType::StreamChunk | MessageType::Delta);
    let (flags, payload) = if must_compress || raw.len() >= COMPRESSION_THRESHOLD {
        (FLAG_ZSTD, zstd::stream::encode_all(Cursor::new(raw), 3)?)
    } else {
        (0, raw)
    };
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload too large"));
    }
    let mut header = BytesMut::with_capacity(HEADER_LEN);
    header.put_u32(payload.len() as u32);
    header.put_u8(flags);
    header.put_u8(ty as u8);
    header.put_u64(request_id);
    writer.write_all(&header).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, ProtocolError> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let mut h = &header[..];
    let len = h.get_u32() as usize;
    let flags = h.get_u8();
    let message_type = MessageType::try_from(h.get_u8())?;
    let request_id = h.get_u64();
    if len > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload too large"));
    }
    if flags & !FLAG_ZSTD != 0 {
        return Err(ProtocolError::Invalid("unknown compression flag"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    let compressed = flags & FLAG_ZSTD != 0;
    if compressed {
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(Cursor::new(payload))?
            .take((MAX_PAYLOAD + 1) as u64)
            .read_to_end(&mut decoded)?;
        payload = decoded;
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::Invalid("decompressed payload too large"));
        }
    }
    Ok(Frame {
        compressed,
        message_type,
        request_id,
        payload,
    })
}
