//! Broker-to-client responses.

use serde::{Deserialize, Serialize};

use crate::account::{Permission, Role, TokenUsage, UserAccount};
use crate::character::Character;
use crate::conversation::{Conversation, ConversationView};
use crate::lore::LoreEntry;
use crate::memory::MemoryEntry;

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
    Users(Vec<UserAccount>),
    BrokerConfig(crate::admin::BrokerConfig),
    BrokerMonitor(crate::admin::BrokerMonitor),
    OllamaState(crate::admin::OllamaState),
    AdminDatabase(Vec<crate::admin::AdminDataRow>),
    ServerCapabilities {
        registration_enabled: bool,
    },
    Permissions(Vec<Permission>),
    Conversations(Vec<Conversation>),
    ConversationView(ConversationView),
    Lore(Vec<LoreEntry>),
    Memories(Vec<MemoryEntry>),
    SyncComplete {
        revision: i64,
    },
    GenerationStarted {
        message_id: String,
        character_id: String,
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
}
