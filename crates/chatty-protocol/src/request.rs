//! Client-to-broker requests.

use serde::{Deserialize, Serialize};

use crate::account::Role;
use crate::character::CharacterInput;
use crate::conversation::{ConversationKind, EntityKind};
use crate::memory::MemoryInput;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Request {
    Register {
        username: String,
        password: String,
    },
    Login {
        username: String,
        password: String,
    },
    GetServerCapabilities,
    Logout {
        session_token: String,
    },
    AdminListUsers {
        session_token: String,
    },
    AdminCreateUser {
        session_token: String,
        username: String,
        password: String,
        role: Role,
    },
    AdminDeleteUser {
        session_token: String,
        user_id: String,
    },
    AdminSetRole {
        session_token: String,
        user_id: String,
        role: Role,
    },
    AdminGetBrokerConfig {
        session_token: String,
    },
    AdminGetBrokerMonitor {
        session_token: String,
    },
    AdminSoftReboot {
        session_token: String,
    },
    AdminSetBrokerConfig {
        session_token: String,
        config: crate::admin::BrokerConfig,
    },
    AdminGetOllamaState {
        session_token: String,
    },
    AdminOllamaAction {
        session_token: String,
        action: crate::admin::OllamaAction,
    },
    AdminSetCharacterPublic {
        session_token: String,
        character_id: String,
        is_public: bool,
    },
    AdminReadDatabase {
        session_token: String,
    },
    GetPermissions {
        session_token: String,
    },
    Resume {
        session_token: String,
        since_revision: i64,
    },
    Snapshot {
        session_token: String,
    },
    ListCharacters {
        session_token: String,
    },
    ListConversations {
        session_token: String,
    },
    GetConversation {
        session_token: String,
        conversation_id: String,
    },
    ListMemories {
        session_token: String,
        conversation_id: Option<String>,
        character_id: Option<String>,
    },
    UpsertCharacter {
        session_token: String,
        character: Box<CharacterInput>,
        /// Owner-scoped worlds that should link to this character after save.
        world_ids: Vec<String>,
    },
    CreateConversation {
        session_token: String,
        title: String,
        kind: ConversationKind,
        participant_ids: Vec<String>,
    },
    UpdateConversation {
        session_token: String,
        conversation_id: String,
        title: String,
        participant_ids: Vec<String>,
    },
    UpdateConversationState {
        session_token: String,
        conversation_id: String,
        state: String,
        summary: String,
    },
    DeleteEntity {
        session_token: String,
        kind: EntityKind,
        entity_id: String,
    },
    SendMessage {
        session_token: String,
        conversation_id: String,
        content: String,
        speaker_id: Option<String>,
    },
    SendSystemMessage {
        session_token: String,
        conversation_id: String,
        content: String,
    },
    SelectVariant {
        session_token: String,
        message_id: String,
        variant_id: String,
    },
    UpsertMemory {
        session_token: String,
        memory: MemoryInput,
    },
    Generate {
        session_token: String,
        conversation_id: String,
        speaker_id: Option<String>,
        parent_id: Option<String>,
    },
    Sync {
        session_token: String,
        since_revision: i64,
    },
    /// Ask the external model to extract one durable fact from recent conversation history.
    /// The broker validates and persists the result; no model output is trusted directly.
    ExtractMemory {
        session_token: String,
        conversation_id: String,
        character_id: Option<String>,
    },
    GetAccountUsage {
        session_token: String,
    },
    ListWorlds {
        session_token: String,
    },
    /// Convert an exported SillyTavern lorebook into an unsaved Chatty world draft.
    ImportSillyTavernWorld {
        session_token: String,
        source_name: String,
        lorebook_json: String,
    },
    SaveWorld {
        session_token: String,
        world: crate::world::World,
    },
    DeleteWorld {
        session_token: String,
        world_id: String,
    },
}
