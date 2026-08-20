//! Ports (hexagonal): the traits the application depends on, with test doubles.
//!
//! Adapters implement these traits; domain and application code never perform
//! I/O directly. Each port ships with a test double so the orchestrator and
//! role executor stay unit-testable without a network, git, or a filesystem.
//! See `docs/role-graph.md` §Hexagonal architecture and ADR-007.
//!
//! Additional ports (`ToolRunner`, `ArtifactStore`, `StateStore`,
//! `ContextProvider`, `Clock`) are introduced with their first consumer rather
//! than speculatively, so each trait's shape is driven by real usage.

use crate::error::Error;

/// One LLM completion request for a single role call.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateRequest {
    /// Model tag/variant to invoke (passed through verbatim).
    pub model: String,
    /// Optional system prompt carrying the role's instructions.
    pub system: Option<String>,
    /// The user prompt (the task plus assembled context).
    pub prompt: String,
    /// Sampling temperature.
    pub temperature: f32,
}

/// Generates text for one role call.
///
/// Adapters (for example an `llm-kernel` wrapper) implement this trait; the
/// application depends only on [`ModelClient`], never on a concrete provider.
#[async_trait::async_trait]
pub trait ModelClient: Send + Sync {
    /// Run one completion and return the model's text.
    async fn generate(&self, request: &GenerateRequest) -> Result<String, Error>;

    /// Redacted origin for run-log context, or `None` to omit the line.
    fn redacted_origin(&self) -> Option<String> {
        None
    }
}

/// Test double for any [`ModelClient`]: returns queued replies, records calls.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ScriptedModelClient {
    replies: std::sync::Mutex<std::collections::VecDeque<Result<String, Error>>>,
    calls: std::sync::Mutex<Vec<GenerateRequest>>,
}

#[cfg(test)]
impl ScriptedModelClient {
    /// Empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a successful reply.
    pub fn push_text(&self, text: impl Into<String>) {
        self.replies
            .lock()
            .expect("script mutex")
            .push_back(Ok(text.into()));
    }

    /// Push a failed generate.
    pub fn push_err(&self, error: Error) {
        self.replies
            .lock()
            .expect("script mutex")
            .push_back(Err(error));
    }

    /// Recorded requests, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<GenerateRequest> {
        self.calls.lock().expect("script mutex").clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl ModelClient for ScriptedModelClient {
    async fn generate(&self, request: &GenerateRequest) -> Result<String, Error> {
        self.calls
            .lock()
            .expect("script mutex")
            .push(request.clone());
        self.replies
            .lock()
            .expect("script mutex")
            .pop_front()
            .unwrap_or_else(|| Err(Error::Artifact("no scripted replies remaining".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> GenerateRequest {
        GenerateRequest {
            model: "phi4".into(),
            system: Some("You are QA.".into()),
            prompt: "review the patch".into(),
            temperature: 0.2,
        }
    }

    #[tokio::test]
    async fn scripted_client_returns_queued_text_and_records_call() {
        let client = ScriptedModelClient::new();
        client.push_text("looks good");
        let text = client.generate(&request()).await.expect("reply");
        assert_eq!(text, "looks good");
        let calls = client.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "phi4");
        assert_eq!(calls[0].prompt, "review the patch");
        assert!((calls[0].temperature - 0.2).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn scripted_client_returns_queued_error() {
        let client = ScriptedModelClient::new();
        client.push_err(Error::Timeout);
        let err = client
            .generate(&request())
            .await
            .expect_err("scripted error");
        assert!(matches!(err, Error::Timeout));
    }

    #[tokio::test]
    async fn scripted_client_exhausted_replies_errors() {
        let client = ScriptedModelClient::new();
        let err = client.generate(&request()).await.expect_err("exhausted");
        assert!(err.to_string().contains("no scripted replies"), "{err}");
    }
}
