//! Injectable text generation (Ollama in production, scripted in tests).

use crate::error::Error;
use crate::ollama::OllamaClient;
use async_trait::async_trait;
#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;

/// Default sampling temperature for PM, Tech Lead, Dev, and QA.
pub const ROLE_TEMPERATURE: f32 = 0.2;

/// Sequential text completion.
#[async_trait]
pub trait Generator: Send + Sync {
    /// Complete `prompt` with `model` at `temperature`.
    async fn generate(&self, model: &str, prompt: &str, temperature: f32) -> Result<String, Error>;
}

#[async_trait]
impl Generator for OllamaClient {
    async fn generate(&self, model: &str, prompt: &str, temperature: f32) -> Result<String, Error> {
        self.generate_with(model, prompt, temperature).await
    }
}

/// One recorded [`Generator::generate`] invocation.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateCall {
    /// Model tag passed to generate.
    pub model: String,
    /// Prompt text.
    pub prompt: String,
    /// Sampling temperature.
    pub temperature: f32,
}

/// Queue of scripted replies for tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ScriptedGenerator {
    replies: Mutex<VecDeque<Result<String, Error>>>,
    calls: Mutex<Vec<GenerateCall>>,
}

#[cfg(test)]
impl ScriptedGenerator {
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

    /// Recorded calls, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<GenerateCall> {
        self.calls.lock().expect("script mutex").clone()
    }
}

#[cfg(test)]
#[async_trait]
impl Generator for ScriptedGenerator {
    async fn generate(&self, model: &str, prompt: &str, temperature: f32) -> Result<String, Error> {
        self.calls.lock().expect("script mutex").push(GenerateCall {
            model: model.to_string(),
            prompt: prompt.to_string(),
            temperature,
        });
        self.replies
            .lock()
            .expect("script mutex")
            .pop_front()
            .unwrap_or_else(|| {
                Err(Error::Ollama(
                    "ScriptedGenerator: no remaining replies".into(),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_returns_queued_text_and_records_call() {
        let gen = ScriptedGenerator::new();
        gen.push_text("hello");
        let text = gen
            .generate("llama3.1:8b", "goal", ROLE_TEMPERATURE)
            .await
            .expect("reply");
        assert_eq!(text, "hello");
        let calls = gen.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "llama3.1:8b");
        assert_eq!(calls[0].prompt, "goal");
        assert!((calls[0].temperature - ROLE_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn scripted_push_err_is_returned() {
        let gen = ScriptedGenerator::new();
        gen.push_err(Error::Timeout);
        let err = gen
            .generate("m", "p", ROLE_TEMPERATURE)
            .await
            .expect_err("scripted error");
        assert!(matches!(err, Error::Timeout));
    }
}
