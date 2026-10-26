//! OpenAI-compatible WASM entry point.
#[path = "../../shared/api.rs"]
pub mod adapter_api;
#[path = "../../shared/openai_compatible.rs"]
mod openai_compatible;
pub use openai_compatible::translate;
crate::register_adapter!(translate);
