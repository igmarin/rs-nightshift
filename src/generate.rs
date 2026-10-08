//! Injectable text generation built on `llm-kernel`'s `LLMClient` trait.
//!
//! Production uses [`crate::adapters::ollama::OllamaClient`] (an `LLMClient` impl that
//! talks to a local Ollama origin and unloads the model after each call).
//! Tests use `ScriptedGenerator` (now in [`crate::adapters::providers`]),
//! another `LLMClient` impl that returns queued replies.

use crate::adapters::kernel_error::map_kernel_error;
use crate::error::Error;
pub use llm_kernel::llm::LLMClient;
use llm_kernel::llm::{ChatMessage, LLMRequest, LLMResponse};

/// Default sampling temperature for PM, Tech Lead, Dev, and QA.
pub const ROLE_TEMPERATURE: f32 = 0.2;

/// Sampling temperature for the Writer article draft.
pub const WRITER_TEMPERATURE: f32 = 0.5;

/// Origin label for operator-facing run logs, when the client can report one.
///
/// Implemented by [`crate::adapters::ollama::OllamaClient`] (redacted Ollama URL) and
/// trivially by any test double that wants to surface an origin line.
pub trait Origin: LLMClient {
    /// Redacted origin for run-log context, or `None` to omit the line.
    fn redacted_origin(&self) -> Option<String> {
        None
    }
}

/// Run a single-prompt completion against `client` and return the text.
///
/// This is the shared call-site helper used by every stage: it builds an
/// [`LLMRequest`] from a user prompt + temperature, calls
/// [`LLMClient::complete`], and unwraps the response content. Kernel errors
/// are mapped via [`map_kernel_error`] with the requested model tag.
pub async fn complete_text(
    client: &dyn LLMClient,
    model: &str,
    prompt: &str,
    temperature: f32,
) -> Result<String, Error> {
    let request = LLMRequest {
        model: Some(model.to_string()),
        messages: vec![ChatMessage::user(prompt)],
        temperature,
        ..LLMRequest::default()
    };
    let response: LLMResponse = client
        .complete(request)
        .await
        .map_err(|e| map_kernel_error(e, model))?;
    Ok(response.content)
}

// Backward-compat re-export: `ScriptedGenerator` and `GenerateCall` were moved
// to `adapters::providers::scripted` (issue #83) but legacy stage test modules
// still import them from `crate::generate`. This re-export keeps those imports
// working without touching every test file; new code should import from
// `crate::adapters::providers` directly.
#[cfg(test)]
pub use crate::adapters::providers::{GenerateCall, ScriptedGenerator};
