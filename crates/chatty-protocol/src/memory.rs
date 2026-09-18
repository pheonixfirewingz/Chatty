//! Durable, character-scoped memories.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryKind {
    #[default]
    Fact,
    Event,
    Relationship,
    Reflection,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemorySource {
    #[default]
    Manual,
    Automatic,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fact => "Fact",
            Self::Event => "Event",
            Self::Relationship => "Relationship",
            Self::Reflection => "Reflection",
        }
    }
}

impl MemorySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Manual",
            Self::Automatic => "Learned",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryInput {
    pub id: Option<String>,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub kind: MemoryKind,
    pub content: String,
    pub importance: u8,
    pub pinned: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub kind: MemoryKind,
    pub content: String,
    pub importance: u8,
    pub pinned: bool,
    pub confidence: f32,
    pub source: MemorySource,
    pub source_message_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}
