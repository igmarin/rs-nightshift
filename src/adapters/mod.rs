//! Adapters (hexagonal) implementing the ports (ADR-007).
//!
//! This is the only layer allowed to import `llm-kernel` and `reqwest`
//! (ADR-007; `docs/role-graph.md` §Hexagonal). Provider adapters (Ollama,
//! OpenAI-compatible, and the factory) live in [`providers`]; the remaining
//! modules are filesystem/state/clock/capability adapters.

pub mod artifact_store;
pub mod capabilities;
pub mod clock;
pub mod kernel_error;
pub mod providers;
pub mod state;

pub use providers::{
    build_model_client, OllamaAdapter, OpenAICompatibleAdapter, ProviderFactory,
    DEFAULT_DEEPSEEK_API_KEY_ENV, DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_KIMI_API_KEY_ENV,
    DEFAULT_KIMI_BASE_URL, DEFAULT_OLLAMA_BASE_URL,
};
