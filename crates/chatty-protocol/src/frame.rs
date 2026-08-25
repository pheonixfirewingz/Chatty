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

const COMPRESSION_LEVEL: i32 = 3;
/// Upper bound slack for `zstd` worst-case expansion: `n + n/255 + small`.
/// `n/8 + 1024` covers every input up to and beyond `MAX_PAYLOAD`.
const COMPRESS_BOUND_SLACK: usize = 1024;

/// Reusable per-connection codec state: persistent zstd contexts plus scratch
/// buffers, so framing a message does not allocate fresh contexts or emit a
/// separate header write per frame. Wire output is identical to the free
/// functions above.
pub struct ProtocolCodec {
    compressor: zstd::bulk::Compressor<'static>,
    decompressor: zstd::bulk::Decompressor<'static>,
    tx_scratch: BytesMut,
    compress_scratch: Vec<u8>,
    decompress_scratch: Vec<u8>,
    rx_scratch: BytesMut,
}

impl Default for ProtocolCodec {
    fn default() -> Self {
        Self::new().expect("zstd contexts are always constructible")
    }
}

impl ProtocolCodec {
    pub fn new() -> Result<Self, ProtocolError> {
        Ok(Self {
            compressor: zstd::bulk::Compressor::new(COMPRESSION_LEVEL)
                .map_err(|e| ProtocolError::Codec(e.to_string()))?,
            decompressor: zstd::bulk::Decompressor::new()
                .map_err(|e| ProtocolError::Codec(e.to_string()))?,
            tx_scratch: BytesMut::with_capacity(16 * 1024),
            compress_scratch: Vec::new(),
            decompress_scratch: Vec::new(),
            rx_scratch: BytesMut::with_capacity(16 * 1024),
        })
    }

    pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
        &mut self,
        writer: &mut W,
        ty: MessageType,
        request_id: u64,
        value: &T,
    ) -> Result<(), ProtocolError> {
        let raw = encode(value)?;
        self.write_payload(writer, ty, request_id, &raw).await
    }

    /// Frames an already-bincode-encoded payload through reused buffers.
    /// Header and payload leave in a single `write_all`.
    pub async fn write_payload<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
        ty: MessageType,
        request_id: u64,
        payload: &[u8],
    ) -> Result<(), ProtocolError> {
        let must_compress = matches!(ty, MessageType::StreamChunk | MessageType::Delta);
        self.tx_scratch.clear();
        if must_compress || payload.len() >= COMPRESSION_THRESHOLD {
            let bound = payload.len() + payload.len() / 8 + COMPRESS_BOUND_SLACK;
            self.compress_scratch.resize(bound, 0);
            let written = self
                .compressor
                .compress_to_buffer(payload, &mut self.compress_scratch)
                .map_err(|e| ProtocolError::Codec(e.to_string()))?;
            if written > MAX_PAYLOAD {
                return Err(ProtocolError::Invalid("payload too large"));
            }
            let out = &mut self.tx_scratch;
            out.reserve(HEADER_LEN + written);
            out.put_u32(written as u32);
            out.put_u8(FLAG_ZSTD);
            out.put_u8(ty as u8);
            out.put_u64(request_id);
            out.extend_from_slice(&self.compress_scratch[..written]);
        } else {
            if payload.len() > MAX_PAYLOAD {
                return Err(ProtocolError::Invalid("payload too large"));
            }
            let out = &mut self.tx_scratch;
            out.reserve(HEADER_LEN + payload.len());
            out.put_u32(payload.len() as u32);
            out.put_u8(0);
            out.put_u8(ty as u8);
            out.put_u64(request_id);
            out.extend_from_slice(payload);
        }
        writer.write_all(&self.tx_scratch).await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn read_frame<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Frame, ProtocolError> {
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
        let compressed = flags & FLAG_ZSTD != 0;
        self.rx_scratch.clear();
        self.rx_scratch.reserve(len);
        while self.rx_scratch.len() < len {
            let n = reader.read_buf(&mut self.rx_scratch).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "frame payload truncated",
                )
                .into());
            }
        }
        let payload = if compressed {
            let ProtocolCodec {
                compressor: _,
                decompressor,
                tx_scratch: _,
                compress_scratch: _,
                decompress_scratch,
                rx_scratch,
            } = self;
            decompress_via(decompressor, decompress_scratch, &rx_scratch[..len])?
        } else {
            self.rx_scratch[..len].to_vec()
        };
        Ok(Frame {
            compressed,
            message_type,
            request_id,
            payload,
        })
    }
}

fn decompress_via(
    decompressor: &mut zstd::bulk::Decompressor<'static>,
    scratch: &mut Vec<u8>,
    src: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    let first_guess = (src.len().saturating_mul(6)).clamp(COMPRESSION_THRESHOLD, MAX_PAYLOAD);
    for cap in [first_guess, MAX_PAYLOAD] {
        scratch.resize(cap, 0);
        match decompressor.decompress_to_buffer(src, scratch) {
            Ok(written) => return Ok(scratch[..written].to_vec()),
            Err(_) => continue,
        }
    }
    let mut decoded = Vec::new();
    zstd::stream::read::Decoder::new(Cursor::new(src))?
        .take((MAX_PAYLOAD + 1) as u64)
        .read_to_end(&mut decoded)?;
    if decoded.len() > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("decompressed payload too large"));
    }
    Ok(decoded)
}
