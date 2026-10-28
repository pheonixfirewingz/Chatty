//! Broker configuration, monitoring, and Ollama management messages.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct BrokerConfig {
    pub adapter_enabled: bool,
    pub adapter_url: String,
    /// Installed inference plugin ID. Empty preserves the legacy API selection.
    pub adapter_plugin: String,
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
    pub image: ImageConfig,
    /// Broker-host path to the emotion classifier GGUF, run in-process after
    /// each reply to pick a portrait. Empty disables classification.
    pub emotion_model: String,
}

/// Text-to-image service used for portraits and scene images. It is separate
/// from the text inference adapter and is disabled until configured.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ImageConfig {
    pub enabled: bool,
    pub url: String,
    /// Installed image plugin ID, such as `comfyui`.
    pub plugin: String,
    /// Server model or checkpoint name. Empty selects the first listed model.
    pub model: String,
    /// Default output size for scene images; portraits are square.
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub guidance: f32,
    /// Appended to every request's negative prompt.
    pub negative_prompt: String,
    /// API-format workflow template with `{{placeholder}}` inputs. Empty uses
    /// the plugin's built-in workflow.
    pub workflow: String,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://127.0.0.1:8188".into(),
            plugin: "comfyui".into(),
            model: String::new(),
            width: 1024,
            height: 1024,
            steps: 25,
            guidance: 7.0,
            negative_prompt: String::new(),
            workflow: String::new(),
        }
    }
}

/// Inclusive output bounds; dimensions are multiples of eight.
pub const MIN_IMAGE_SIDE: u32 = 256;
pub const MAX_IMAGE_SIDE: u32 = 2048;
pub const MAX_IMAGE_STEPS: u32 = 150;
pub const MAX_IMAGE_PROMPT_BYTES: usize = 4096;
pub const MAX_IMAGE_WORKFLOW_BYTES: usize = 256 * 1024;
/// Values the broker fills into a workflow template for each request.
pub const IMAGE_WORKFLOW_PLACEHOLDERS: [&str; 8] = [
    "{{prompt}}",
    "{{negative_prompt}}",
    "{{model}}",
    "{{seed}}",
    "{{width}}",
    "{{height}}",
    "{{steps}}",
    "{{guidance}}",
];

impl BrokerConfig {
    /// Resolve settings written before explicit plugin selection was available.
    pub fn adapter_plugin_id(&self) -> &str {
        if self.adapter_plugin.is_empty() {
            if self.use_ollama_api {
                "ollama"
            } else {
                "openai-compatible"
            }
        } else {
            &self.adapter_plugin
        }
    }
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

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct BrokerLog {
    /// Newest UTF-8 log text available within the response size bound.
    pub content: String,
    /// True when older lines from this boot were omitted from the response.
    pub truncated: bool,
    pub file_size_bytes: u64,
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

/// Public GGUF repository and bounded downloadable file choices from Hugging Face.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelSearchResult {
    pub id: String,
    pub downloads: u64,
    pub files: Vec<String>,
}

/// Progress for the current download layer, not a whole-model percentage.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ModelDownloadProgress {
    pub status: String,
    pub digest: Option<String>,
    pub total: Option<u64>,
    pub completed: Option<u64>,
}

/// Bounds concurrent pulls in both the broker and the GUI.
pub const MAX_PARALLEL_DOWNLOADS: usize = 4;
