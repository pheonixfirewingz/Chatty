//! Error types shared by every protocol message.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid frame: {0}")]
    Invalid(&'static str),
    #[error("codec error: {0}")]
    Codec(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    InvalidRequest,
    NotFound,
    Conflict,
    BackendUnavailable,
    ModelMissing,
    CorruptFrame,
    Internal,
}
