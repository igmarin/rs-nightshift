//! Per-role option parsing shared by the provider adapters.

use crate::error::{ConfigError, Error};
use std::collections::BTreeMap;

/// Resolved per-role options that the adapters act on.
///
/// `temperature` and `max_tokens` apply to every provider; `think` is only
/// acted on by the Ollama adapter (`:think` tag suffix). `think_explicitly_false`
/// tracks whether `think = false` was explicitly set (vs absent), which
/// triggers the native `/api/chat` path with `think: false`. `num_ctx` is
/// parsed and validated but deliberately not stored (llm-kernel's
/// OpenAI-compatible request has no `num_ctx` field).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ModelOptions {
    /// Overrides the request temperature when set.
    pub(crate) temperature: Option<f32>,
    /// Requested max output tokens.
    pub(crate) max_tokens: Option<u32>,
    /// Whether the Ollama `:think` tag suffix should be applied.
    pub(crate) think: bool,
    /// Whether `think = false` was explicitly set (vs absent).
    pub(crate) think_explicitly_false: bool,
}

impl ModelOptions {
    /// Parse and type-check the role's `options` map.
    pub(crate) fn parse(options: &BTreeMap<String, toml::Value>) -> Result<Self, Error> {
        let temperature = match options.get("temperature") {
            None => None,
            Some(toml::Value::Float(value)) => Some(*value as f32),
            Some(toml::Value::Integer(value)) => Some(*value as f32),
            Some(_) => return Err(bad_option("temperature", "a number")),
        };
        if let Some(value) = temperature {
            if !(0.0..=2.0).contains(&value) {
                return Err(bad_option("temperature", "a number between 0.0 and 2.0"));
            }
        }
        let max_tokens = match options.get("max_tokens") {
            None => None,
            Some(value) => {
                let value = value
                    .as_integer()
                    .ok_or_else(|| bad_option("max_tokens", "an integer"))?;
                Some(
                    u32::try_from(value)
                        .map_err(|_| bad_option("max_tokens", "a non-negative integer"))?,
                )
            }
        };
        let think = match options.get("think") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| bad_option("think", "a boolean"))?,
        };
        let think_explicitly_false = options
            .get("think")
            .is_some_and(|v| v.as_bool() == Some(false));
        if let Some(value) = options.get("num_ctx") {
            let value = value
                .as_integer()
                .ok_or_else(|| bad_option("num_ctx", "an integer"))?;
            // Validated but not forwarded: see the module docs.
            u32::try_from(value).map_err(|_| bad_option("num_ctx", "a non-negative integer"))?;
        }
        Ok(Self {
            temperature,
            max_tokens,
            think,
            think_explicitly_false,
        })
    }
}

/// Build an [`Error::Config`] for a malformed role option.
fn bad_option(name: &str, expected: &str) -> Error {
    Error::from(ConfigError {
        path: format!("option {name:?}"),
        message: format!("expected {expected}"),
    })
}
