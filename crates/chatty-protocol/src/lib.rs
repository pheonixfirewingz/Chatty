//! Wire contract shared by the broker and native clients.
//! The fixed header is: payload length (u32 BE), flags (u8), type (u8), request id (u64 BE).

pub mod account;
pub mod admin;
pub mod character;
pub mod conversation;
pub mod delta;
pub mod error;
pub mod frame;
pub mod lore;
pub mod memory;
pub mod request;
pub mod response;
pub mod util;

pub use account::{Permission, Role, TokenUsage, UserAccount};
pub use admin::{
    AdapterStatus, AdminDataRow, BrokerConfig, BrokerMonitor, OllamaAction, OllamaModel,
    OllamaRunningModel, OllamaState,
};
pub use character::{Character, CharacterInput};
pub use conversation::{
    ChatMessage, Conversation, ConversationKind, ConversationView, EntityKind, Variant,
};
pub use delta::{DeltaOperation, DeltaPayload, StateDelta, StreamChunk};
pub use error::{ErrorCode, ProtocolError, WireError};
pub use frame::{
    COMPRESSION_THRESHOLD, FLAG_ZSTD, Frame, HEADER_LEN, MAX_PAYLOAD, MessageType, decode, encode,
    read_frame, write_message, write_payload,
};
pub use lore::{LoreEntry, LoreInput};
pub use memory::{MemoryEntry, MemoryInput};
pub use request::Request;
pub use response::Response;
pub use util::current_utc_timestamp;

#[cfg(test)]
mod tests;
