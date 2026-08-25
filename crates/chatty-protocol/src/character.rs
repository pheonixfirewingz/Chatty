//! Character cards, including Chatty extensions to the SillyTavern spec.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CharacterInput {
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub personality: String,
    pub scenario: String,
    pub system_prompt: String,
    pub example_dialogue: String,
    pub appearance: String,
    /// Chatty extensions beyond the SillyTavern character card spec.
    #[serde(default)]
    pub age: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub race: String,
    #[serde(default)]
    pub misc: String,
    pub tags: Vec<String>,
    pub avatar: Option<Vec<u8>>,
    pub is_public: bool,
    /// Set by the broker in character responses. Clients must not use this to
    /// claim ownership when creating or updating a character.
    pub owned_by_user: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Character {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub personality: String,
    pub scenario: String,
    pub system_prompt: String,
    pub example_dialogue: String,
    pub appearance: String,
    /// Chatty extensions beyond the SillyTavern character card spec.
    #[serde(default)]
    pub age: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub race: String,
    #[serde(default)]
    pub misc: String,
    pub tags: Vec<String>,
    pub avatar: Option<Vec<u8>>,
    pub is_public: bool,
    pub owned_by_user: bool,
    pub revision: i64,
}
