//! Provider adapters implementing the [`ModelClient`](crate::ports::ModelClient)
//! port (ADR-007).
//!
//! This is the only layer allowed to import `llm-kernel` and `reqwest`. It
//! provides an OpenAI-compatible adapter (Deepseek, Kimi, and any custom
//! `openai-compatible` provider) and an Ollama adapter that preserves the
//! `keep_alive: 0` VRAM-unload behavior, plus a factory that wires provider
//! names + `ProviderSpec` + role options into a concrete client.
//!
//! # Provider rules
//!
//! - `ollama` — local, default base URL `DEFAULT_OLLAMA_BASE_URL`; no API key
//!   (a placeholder is passed to `llm-kernel`'s OpenAI client, mirroring
//!   `OllamaClient`). The `think` option appends the `:think` model-tag suffix
//!   (Ollama's convention for thinking variants, e.g. `qwen3:think`), so
//!   `options.think = true` on role `model = "phi4"` calls `phi4:think`.
//!   `num_ctx` is validated but deliberately not forwarded: llm-kernel's
//!   OpenAI-compatible request has no `num_ctx` field, so the value cannot
//!   reach Ollama through this path.
//! - `deepseek` — OpenAI-compatible, default base URL `DEFAULT_DEEPSEEK_BASE_URL`,
//!   key from `DEFAULT_DEEPSEEK_API_KEY_ENV`. The model tag is passed verbatim
//!   (e.g. `deepseek-v4-pro`, or `deepseek-reasoner` for high-thinking — the
//!   operator owns the tag).
//! - `kimi` — OpenAI-compatible, default base URL `DEFAULT_KIMI_BASE_URL`, key
//!   from `DEFAULT_KIMI_API_KEY_ENV`.
//! - any other provider name — resolved through `ProviderSpec.backend`;
//!   `openai-compatible` is honored, anything else is a config error.
//!
//! # Options (`RoleSpec.options`)
//!
//! `temperature` (float, or integer) overrides the request temperature when
//! present; `max_tokens` (non-negative integer) caps the completion length;
//! `think` (boolean) is honored by the Ollama adapter (see above). Unknown
//! option keys are ignored (the config schema declares options a
//! provider/model-specific passthrough). Invalid option *types* are config
//! errors so typos surface at build time rather than silently.

mod factory;
mod ollama;
mod openai;
mod options;

pub use factory::{
    build_model_client, ProviderFactory, DEFAULT_DEEPSEEK_API_KEY_ENV, DEFAULT_DEEPSEEK_BASE_URL,
    DEFAULT_KIMI_API_KEY_ENV, DEFAULT_KIMI_BASE_URL, DEFAULT_OLLAMA_BASE_URL,
};
pub use ollama::OllamaAdapter;
pub use openai::OpenAICompatibleAdapter;

#[cfg(test)]
mod test_support;
