//! Broker configuration, monitoring, and Ollama management messages.

use serde::{Deserialize, Serialize};

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
