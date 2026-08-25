//! Error types shared by every protocol message.

use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use std::fmt;

/// Hand-written in place of a `thiserror` derive: `Display` text per variant
/// plus the standard `source()`/`From<io::Error>` plumbing.
#[derive(Debug)]
pub enum ProtocolError {
    Io(std::io::Error),
    Invalid(&'static str),
    Codec(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Invalid(reason) => write!(f, "invalid frame: {reason}"),
            Self::Codec(reason) => write!(f, "codec error: {reason}"),
        }
    }
}

impl StdError for ProtocolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
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
