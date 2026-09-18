//! Broker-to-client responses.

use serde::{Deserialize, Serialize};

use crate::account::{Permission, Role, TokenUsage, UserAccount};
use crate::character::{Character, CharacterInput};
use crate::conversation::{Conversation, ConversationView};
use crate::memory::MemoryEntry;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStatus {
    SelectingEmotion,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Response {
    Authenticated {
        session_token: String,
        user_id: String,
        role: Role,
        revision: i64,
    },
    Accepted {
        entity_id: Option<String>,
        revision: i64,
    },
    Characters(Vec<Character>),
    CharacterImage {
        character_id: String,
        image_id: String,
        data: Vec<u8>,
    },
    Users(Vec<UserAccount>),
    BrokerConfig(crate::admin::BrokerConfig),
    BrokerMonitor(crate::admin::BrokerMonitor),
    BrokerLog(crate::admin::BrokerLog),
    OllamaState(crate::admin::OllamaState),
    AdminDatabase(Vec<crate::admin::AdminDataRow>),
    ServerCapabilities {
        registration_enabled: bool,
    },
    Permissions(Vec<Permission>),
    Conversations(Vec<Conversation>),
    ConversationView(ConversationView),
    Memories(Vec<MemoryEntry>),
    SyncComplete {
        revision: i64,
    },
    GenerationStarted {
        message_id: String,
        character_id: String,
    },
    GenerationStatus {
        message_id: String,
        status: GenerationStatus,
    },
    GenerationFinished {
        message_id: String,
        revision: i64,
        cancelled: bool,
    },
    Pong,
    /// The requested conversation disappeared before it could be opened.
    /// This is an expected read outcome, not a protocol error.
    ConversationNotFound {
        conversation_id: String,
    },
    AccountUsage(TokenUsage),
    Worlds(Vec<crate::world::World>),
    /// AI-converted world returned for review; the broker has not persisted it.
    WorldImportPreview(crate::world::World),
    /// AI-inferred character fields returned for review; the broker has not persisted it.
    CharacterImportPreview(Box<CharacterInput>),
}
