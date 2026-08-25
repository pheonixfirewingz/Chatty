//! Wire contract shared by the broker and native clients.
//! The fixed header is: payload length (u32 BE), flags (u8), type (u8), request id (u64 BE).

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Cursor, Read};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const UTC_TIMESTAMP_FORMAT: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");

pub fn current_utc_timestamp() -> String {
    format_utc_timestamp(time::OffsetDateTime::now_utc())
}

fn format_utc_timestamp(timestamp: time::OffsetDateTime) -> String {
    timestamp
        .format(UTC_TIMESTAMP_FORMAT)
        .unwrap_or_else(|_| "unknown time UTC".to_owned())
}

pub const HEADER_LEN: usize = 14;
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
pub const COMPRESSION_THRESHOLD: usize = 256;
pub const FLAG_ZSTD: u8 = 1;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid frame: {0}")]
    Invalid(&'static str),
    #[error("codec error: {0}")]
    Codec(String),
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Handshake = 1,
    Request = 2,
    Response = 3,
    Delta = 4,
    StreamChunk = 5,
    StreamEnd = 6,
    Error = 7,
    Cancel = 8,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        Ok(match value {
            1 => Self::Handshake,
            2 => Self::Request,
            3 => Self::Response,
            4 => Self::Delta,
            5 => Self::StreamChunk,
            6 => Self::StreamEnd,
            7 => Self::Error,
            8 => Self::Cancel,
            _ => return Err(ProtocolError::Invalid("unknown message type")),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub compressed: bool,
    pub message_type: MessageType,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

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
        config: BrokerConfig,
    },
    AdminGetOllamaState {
        session_token: String,
    },
    AdminOllamaAction {
        session_token: String,
        action: OllamaAction,
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
    ListLore {
        session_token: String,
        conversation_id: Option<String>,
    },
    ListMemories {
        session_token: String,
        conversation_id: Option<String>,
        character_id: Option<String>,
    },
    UpsertCharacter {
        session_token: String,
        character: CharacterInput,
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
    UpsertLore {
        session_token: String,
        lore: LoreInput,
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
}

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
    BrokerConfig(BrokerConfig),
    BrokerMonitor(BrokerMonitor),
    OllamaState(OllamaState),
    AdminDatabase(Vec<AdminDataRow>),
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    InvalidRequest,
    NotFound,
    Conflict,
    BackendUnavailable,
    ModelMissing,
    CorruptFrame,
    Internal,
}

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
pub struct MemoryInput {
    pub id: Option<String>,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub content: String,
}

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
    Character(CharacterInput),
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
    },
    Lore(LoreInput),
    Memory(MemoryInput),
    Variant {
        message_id: String,
        content: String,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub conversation_id: Option<String>,
    pub character_id: Option<String>,
    pub content: String,
    pub revision: i64,
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

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct BrokerConfig {
    pub adapter_enabled: bool,
    pub adapter_url: String,
    /// Use Ollama's native `/api/chat` surface so all runtime options are honored.
    pub use_ollama_api: bool,
    /// Empty selects the first model returned by the adapter.
    pub model: String,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub num_ctx: u32,
    /// `-1` lets Ollama choose; positive values cap generated tokens.
    pub num_predict: i32,
    pub repeat_penalty: f32,
    /// `-1` requests a random seed.
    pub seed: i64,
    /// Ollama duration such as `5m`, `1h`, or `0`.
    pub keep_alive: String,
    pub allow_public_characters: bool,
    pub allow_self_registration: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum OllamaAction {
    Pull { model: String },
    Delete { model: String },
    Load { model: String },
    Unload { model: String },
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct OllamaState {
    pub version: String,
    pub models: Vec<OllamaModel>,
    pub running_models: Vec<OllamaRunningModel>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OllamaModel {
    pub name: String,
    pub size: u64,
    pub modified_at: String,
    pub family: String,
    pub parameter_size: String,
    pub quantization_level: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OllamaRunningModel {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BrokerMonitor {
    pub uptime_seconds: u64,
    pub cpu_percent: f32,
    pub memory_used_mb: u64,
    pub memory_limit_mb: Option<u64>,
    pub active_connections: u32,
    pub adapter_status: AdapterStatus,
    pub adapter_model_count: u32,
    pub adapter_latency_ms: Option<u64>,
    pub recent_errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum AdapterStatus {
    Disabled,
    Online,
    Offline,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AdminDataRow {
    pub kind: String,
    pub id: String,
    pub label: String,
    pub detail: String,
    pub is_public: Option<bool>,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|e| ProtocolError::Codec(e.to_string()))
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map(|x| x.0)
        .map_err(|e| ProtocolError::Codec(e.to_string()))
}

pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    ty: MessageType,
    request_id: u64,
    value: &T,
) -> Result<(), ProtocolError> {
    let raw = encode(value)?;
    write_payload(writer, ty, request_id, raw).await
}

/// Frames an already-bincode-encoded payload. Useful for bounded writer queues.
pub async fn write_payload<W: AsyncWrite + Unpin>(
    writer: &mut W,
    ty: MessageType,
    request_id: u64,
    raw: Vec<u8>,
) -> Result<(), ProtocolError> {
    let must_compress = matches!(ty, MessageType::StreamChunk | MessageType::Delta);
    let (flags, payload) = if must_compress || raw.len() >= COMPRESSION_THRESHOLD {
        (FLAG_ZSTD, zstd::stream::encode_all(Cursor::new(raw), 3)?)
    } else {
        (0, raw)
    };
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload too large"));
    }
    let mut header = BytesMut::with_capacity(HEADER_LEN);
    header.put_u32(payload.len() as u32);
    header.put_u8(flags);
    header.put_u8(ty as u8);
    header.put_u64(request_id);
    writer.write_all(&header).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, ProtocolError> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let mut h = &header[..];
    let len = h.get_u32() as usize;
    let flags = h.get_u8();
    let message_type = MessageType::try_from(h.get_u8())?;
    let request_id = h.get_u64();
    if len > MAX_PAYLOAD {
        return Err(ProtocolError::Invalid("payload too large"));
    }
    if flags & !FLAG_ZSTD != 0 {
        return Err(ProtocolError::Invalid("unknown compression flag"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    let compressed = flags & FLAG_ZSTD != 0;
    if compressed {
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(Cursor::new(payload))?
            .take((MAX_PAYLOAD + 1) as u64)
            .read_to_end(&mut decoded)?;
        payload = decoded;
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::Invalid("decompressed payload too large"));
        }
    }
    Ok(Frame {
        compressed,
        message_type,
        request_id,
        payload,
    })
}

#[cfg(test)]
mod tests;
