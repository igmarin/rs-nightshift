//! Error types for rs-nightshift.
//!
//! The top-level [`enum@Error`] is a thin wrapper over domain-specific error types:
//!
//! | Variant | Typed error | Owner |
//! | --- | --- | --- |
//! | [`Error::Provider`] | [`ProviderError`] | LLM-provider adapters |
//! | [`Error::Git`] | [`GitError`] | git adapter |
//! | [`Error::Config`] | [`ConfigError`] | config readers |
//! | [`Error::Artifact`] | [`ArtifactError`] | artifact/state/test adapters |
//!
//! Each sub-error is lifted into [`enum@Error`] via `#[from]`, so adapter functions
//! can return `Result<T, ProviderError>` (or `GitError`, `ConfigError`,
//! `ArtifactError`) and call sites that return `Result<T, Error>` lift
//! automatically with `?`. The `#[error(transparent)]` attribute on each
//! wrapper variant forwards `Display` to the inner type, preserving
//! operator-facing messages verbatim.
//!
//! `Context`, `RoleGraph`, and `Io` remain direct variants because they are
//! cross-cutting (not owned by a single adapter) and small enough that
//! extracting them would add indirection without decoupling benefit.

pub mod artifact;
pub mod config;
pub mod git;
pub mod provider;

pub use artifact::ArtifactError;
pub use config::ConfigError;
pub use git::GitError;
pub use provider::ProviderError;

use thiserror::Error;

/// Recoverable library failure.
///
/// See the [module docs](self) for the wrapper/sub-error layout.
#[derive(Error, Debug)]
pub enum Error {
    /// LLM-provider failure (Ollama, OpenAI-compatible, kernel HTTP layer).
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// Git inspect or apply failed (never commit/push/reset/clean).
    #[error(transparent)]
    Git(#[from] GitError),

    /// `nightshift.toml` or a role-graph config file could not be read/parsed.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Artifact directory, report handling, or schema validation failed.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),

    /// `codegraph` / `graphify` context gathering failed.
    #[error("context tools: {0}")]
    Context(String),

    /// The role-graph configuration is semantically invalid (e.g. an unknown
    /// role id in a routing target, a duplicate role id, or a `start` role
    /// that does not exist).
    #[error("invalid role graph: {0}")]
    RoleGraph(String),

    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts() {
        let error = Error::from(std::io::Error::other("disk"));
        assert!(error.to_string().contains("disk"));
    }

    #[test]
    fn context_error_displays_message() {
        let error = Error::Context("codegraph is not on PATH".into());
        assert_eq!(error.to_string(), "context tools: codegraph is not on PATH");
    }

    #[test]
    fn role_graph_error_displays_message() {
        let error = Error::RoleGraph("unknown target role 'qa'".into());
        assert_eq!(
            error.to_string(),
            "invalid role graph: unknown target role 'qa'"
        );
    }

    #[test]
    fn provider_variant_preserves_inner_message() {
        let top = Error::from(ProviderError::Ollama("boom".into()));
        assert_eq!(top.to_string(), "Ollama request failed: boom");
    }

    #[test]
    fn git_variant_preserves_inner_message() {
        let top = Error::from(GitError::new("dirty"));
        assert_eq!(top.to_string(), "git: dirty");
    }

    #[test]
    fn config_variant_preserves_inner_message() {
        let top = Error::from(ConfigError::new("nightshift.toml", "bad"));
        assert_eq!(top.to_string(), "config error in nightshift.toml: bad");
    }

    #[test]
    fn artifact_variant_preserves_inner_message() {
        let top = Error::from(ArtifactError::artifact("missing"));
        assert_eq!(top.to_string(), "artifact error: missing");
    }
}
