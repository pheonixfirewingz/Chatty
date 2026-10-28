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

/// Context consumed by the latest generation in one conversation.
/// Estimates are used until the provider supplies token counts.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct GenerationContext {
    pub conversation_id: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub context_tokens: u32,
    pub estimated: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Response {
    ModelDownloadProgress(crate::admin::ModelDownloadProgress),
    ModelDownloadFinished {
        cancelled: bool,
    },
    ModelSearchResults(Vec<crate::admin::ModelSearchResult>),
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
        #[serde(with = "crate::byte_vec")]
        data: Vec<u8>,
    },
    Users(Vec<UserAccount>),
    BrokerConfig {
        config: Box<crate::admin::BrokerConfig>,
        /// Installed inference plugins; catalogue-only adapters are excluded.
        adapter_plugins: Vec<String>,
        /// Operator override, which takes precedence over persisted selection.
        adapter_override: Option<String>,
        /// Installed text-to-image plugins.
        image_plugins: Vec<String>,
    },
    BrokerMonitor(crate::admin::BrokerMonitor),
    BrokerLog(crate::admin::BrokerLog),
    OllamaState(crate::admin::OllamaState),
    AdminDatabase(Vec<crate::admin::AdminDataRow>),
    ServerCapabilities {
        registration_enabled: bool,
        image_generation: bool,
    },
    Permissions(Vec<Permission>),
    Conversations(Vec<Conversation>),
    ConversationView(ConversationView),
    Memories(Vec<MemoryEntry>),
    /// Result of merging redundant memories. Counts cover changed scopes only.
    MemoriesCondensed {
        before: usize,
        after: usize,
    },
    SyncComplete {
        revision: i64,
    },
    GenerationStarted {
        message_id: String,
        character_id: String,
        context: GenerationContext,
    },
    GenerationStatus {
        message_id: String,
        status: GenerationStatus,
    },
    GenerationFinished {
        message_id: String,
        revision: i64,
        cancelled: bool,
        context: GenerationContext,
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
    ImageModels(Vec<String>),
    /// Unsaved PNG returned for review.
    GeneratedImage {
        #[serde(with = "crate::byte_vec")]
        data: Vec<u8>,
    },
    MessageImage {
        message_id: String,
        #[serde(with = "crate::byte_vec")]
        data: Vec<u8>,
    },
}
