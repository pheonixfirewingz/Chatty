//! Conversations, chat messages, and variants.

use serde::{Deserialize, Serialize};

use crate::error::ProtocolError;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Direct,
    GroupManual,
    GroupRoundRobin,
    GroupAutomatic,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum EntityKind {
    Character,
    Conversation,
    Message,
    Lore,
    Memory,
}

impl TryFrom<i32> for ConversationKind {
    type Error = ProtocolError;
    fn try_from(value: i32) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Direct),
            1 => Ok(Self::GroupManual),
            2 => Ok(Self::GroupRoundRobin),
            3 => Ok(Self::GroupAutomatic),
            _ => Err(ProtocolError::Invalid("unknown conversation kind")),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub kind: ConversationKind,
    pub participant_ids: Vec<String>,
    pub state: String,
    pub summary: String,
    pub revision: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub author_type: String,
    pub author_id: Option<String>,
    pub content: String,
    pub parent_id: Option<String>,
    pub selected_variant_id: Option<String>,
    pub created_at: String,
    pub revision: i64,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Variant {
    pub id: String,
    pub content: String,
    pub created_at: String,
    pub revision: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConversationView {
    pub conversation: Conversation,
    pub messages: Vec<ChatMessage>,
}
