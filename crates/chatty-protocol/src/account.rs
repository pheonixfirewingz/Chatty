//! Account identity and permission primitives.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Admin,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ManageOwnRoleplay,
    GenerateRoleplay,
    ManageUsers,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserAccount {
    pub id: String,
    pub username: String,
    pub role: Role,
    pub created_at: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl TokenUsage {
    pub fn total(self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}
