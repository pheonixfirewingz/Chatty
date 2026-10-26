//! Version 1 of Chatty's WASM adapter contract.
//!
//! Guests translate neutral operations into HTTP plans and provider replies into
//! neutral results. They have no imports, networking, filesystem, or broker state.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ABI_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationOptions {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub context_tokens: Option<u32>,
    pub max_tokens: Option<i32>,
    pub repeat_penalty: Option<f32>,
    pub seed: Option<i64>,
    pub keep_alive: Option<String>,
    pub disable_reasoning: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub options: GenerationOptions,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Completion {
    pub content: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelAction {
    Pull { model: String },
    Delete { model: String },
    Load { model: String, keep_alive: String },
    Unload { model: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelState {
    pub version: String,
    pub models: Vec<InstalledModel>,
    pub running_models: Vec<RunningModel>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    pub size: u64,
    pub modified_at: String,
    pub family: String,
    pub parameter_size: String,
    pub quantization_level: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningModel {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
    pub expires_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub downloads: u64,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdapterRequest {
    Models { timeout_ms: u64 },
    Complete(ChatRequest),
    Stream(ChatRequest),
    State,
    Action(ModelAction),
    Search { query: String },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResponseKind {
    Models,
    Completion,
    State,
    Action,
    Search,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Delete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
    pub timeout_ms: Option<u64>,
}
impl HttpRequest {
    pub fn get(url: String, timeout_ms: u64) -> Self {
        Self {
            method: HttpMethod::Get,
            url,
            query: Vec::new(),
            body: None,
            timeout_ms: Some(timeout_ms),
        }
    }
    pub fn post(url: String, body: Value, timeout_ms: Option<u64>) -> Self {
        Self {
            method: HttpMethod::Post,
            url,
            query: Vec::new(),
            body: Some(body),
            timeout_ms,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpPlan {
    pub requests: Vec<HttpRequest>,
    pub response: ResponseKind,
    pub stream: bool,
    /// Host clamps this to MAX_MESSAGE_BYTES, regardless of guest request.
    pub response_limit: usize,
    /// Accepted MIME prefixes for a streaming response, checked by the host.
    pub stream_content_types: Vec<String>,
}
impl HttpPlan {
    pub fn new(requests: Vec<HttpRequest>, response: ResponseKind) -> Self {
        Self {
            requests,
            response,
            stream: false,
            response_limit: MAX_MESSAGE_BYTES,
            stream_content_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUpdate {
    pub text: String,
    /// Optional additive JSON metadata; older v1 guests may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub done: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PluginInput {
    Prepare {
        base_url: String,
        request: AdapterRequest,
    },
    Decode {
        kind: ResponseKind,
        responses: Vec<Value>,
    },
    /// Complete newline-terminated records, possibly several at once.
    Stream { records: String },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum PluginOutput {
    Plan(HttpPlan),
    Models(Vec<String>),
    Completion(Completion),
    State(ModelState),
    Action,
    Search(Vec<SearchResult>),
    Stream(StreamUpdate),
}

pub type PluginResult = Result<PluginOutput, String>;

/// Register an import-free core WASM module. Each host session owns an instance.
/// Input and output use UTF-8 JSON; adapter_call returns pointer:u32 | length:u32.
#[macro_export]
macro_rules! register_adapter {
    ($handler:path) => {
        #[cfg(target_arch = "wasm32")]
        mod chatty_wasm_exports {
            use super::*;
            #[unsafe(no_mangle)]
            pub extern "C" fn adapter_abi_version() -> u32 {
                $crate::adapter_api::ABI_VERSION
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn adapter_alloc(len: u32) -> u32 {
                let bytes = vec![0u8; len as usize].into_boxed_slice();
                Box::into_raw(bytes) as *mut u8 as u32
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn adapter_free(ptr: u32, len: u32) {
                // SAFETY: host frees exactly the allocations returned by this module.
                unsafe {
                    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        ptr as *mut u8,
                        len as usize,
                    )));
                }
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn adapter_call(ptr: u32, len: u32) -> u64 {
                // SAFETY: host allocated and initialized this input in guest memory.
                let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
                let reply: $crate::adapter_api::PluginResult =
                    $crate::adapter_api::serde_json::from_slice(bytes)
                        .map_err(|error| error.to_string())
                        .and_then($handler);
                let output = $crate::adapter_api::serde_json::to_vec(&reply)
                    .expect("serializable plugin reply")
                    .into_boxed_slice();
                let length = output.len() as u32;
                let pointer = Box::into_raw(output) as *mut u8 as u32;
                (u64::from(pointer) << 32) | u64::from(length)
            }
        }
    };
}

#[doc(hidden)]
pub use serde_json;
