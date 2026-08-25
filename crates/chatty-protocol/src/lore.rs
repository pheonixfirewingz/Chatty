//! World lore entries.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoreInput {
    pub id: Option<String>,
    pub conversation_id: Option<String>,
    pub keywords: Vec<String>,
    pub content: String,
    pub always_on: bool,
    pub priority: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoreEntry {
    pub id: String,
    pub conversation_id: Option<String>,
    pub keywords: Vec<String>,
    pub content: String,
    pub always_on: bool,
    pub priority: i32,
    pub revision: i64,
}
