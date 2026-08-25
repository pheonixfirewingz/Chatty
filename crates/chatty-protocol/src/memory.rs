//! Durable memory facts extracted by the model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryInput {
    pub id: Option<String>,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub content: String,
    pub revision: i64,
}
