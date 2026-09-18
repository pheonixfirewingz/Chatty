//! State synchronization deltas and streaming payloads.

use serde::{Deserialize, Serialize};

use crate::character::CharacterInput;
use crate::conversation::ConversationKind;
use crate::memory::MemoryEntry;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum DeltaOperation {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StateDelta {
    pub revision: i64,
    pub entity_type: String,
    pub entity_id: String,
    pub operation: DeltaOperation,
    pub changed_fields: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum DeltaPayload {
    Character(Box<CharacterInput>),
    Conversation {
        title: String,
        kind: ConversationKind,
        participant_ids: Vec<String>,
        state: String,
        summary: String,
    },
    ConversationContext {
        state: String,
        summary: String,
    },
    Message {
        conversation_id: String,
        author_type: String,
        author_id: Option<String>,
        content: String,
        parent_id: Option<String>,
        selected_variant_id: Option<String>,
        character_image_id: Option<String>,
    },
    World(crate::world::World),
    Memory(MemoryEntry),
    Variant {
        message_id: String,
        content: String,
        /// Portrait selected for this regenerated reply.
        #[serde(default)]
        character_image_id: Option<String>,
    },
    VariantSelection {
        variant_id: Option<String>,
    },
    Empty,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StreamChunk {
    pub message_id: String,
    pub sequence: u32,
    pub text: String,
}
