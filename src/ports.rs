//! Ports (hexagonal): the traits the application depends on, with test doubles.
//!
//! Adapters implement these traits; domain and application code never perform
//! I/O directly. Each port ships with a test double so the orchestrator and
//! role executor stay unit-testable without a network, git, or a filesystem.
//! See `docs/role-graph.md` §Hexagonal architecture and ADR-007.
//!
//! `ToolRunner` and `ContextProvider` are introduced with their first consumer
//! (the capabilities ticket) rather than speculatively, so each trait's shape
//! is driven by real usage.

use crate::domain::rolegraph::config::ProviderSpec;
use crate::domain::rolegraph::state::{ActionEvent, StatusSnapshot};
use crate::error::Error;
#[cfg(test)]
use crate::error::{ArtifactError, ProviderError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
            .unwrap_or_else(|| {
                Err(Error::from(ArtifactError::artifact(
                    "no scripted replies remaining",
                )))
            })
    }
}

/// A wall clock, abstracted so runs are deterministic in tests.
pub trait Clock: Send + Sync {
    /// Current instant as ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
    fn now_iso(&self) -> String;

    /// Today's calendar date as `YYYY-MM-DD` (the run-dir slug).
    fn today(&self) -> String;
}

/// Test double for [`Clock`]: returns fixed values.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct FixedClock {
    /// The fixed ISO-8601 timestamp to return.
    pub now_iso: String,
    /// The fixed calendar date to return.
    pub today: String,
}

#[cfg(test)]
impl Clock for FixedClock {
    fn now_iso(&self) -> String {
        self.now_iso.clone()
    }

    fn today(&self) -> String {
        self.today.clone()
    }
}

/// Reads and writes role artifacts for a run.
///
/// The filesystem adapter implements this; the executor and orchestrator depend
/// only on this trait, never on `std::fs` directly.
pub trait ArtifactStore: Send + Sync {
    /// Create the run directory and return its path.
    fn create_run(&self, date: &str, slug: &str) -> Result<PathBuf, Error>;

    /// Read an artifact file by name, relative to the run directory.
    fn read_artifact(&self, run: &Path, name: &str) -> Result<String, Error>;

    /// Write an artifact file by name, relative to the run directory.
    fn write_artifact(&self, run: &Path, name: &str, content: &str) -> Result<(), Error>;
}

/// Test double for [`ArtifactStore`]: an in-memory name → content map.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MemoryArtifactStore {
    files: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
}

#[cfg(test)]
impl ArtifactStore for MemoryArtifactStore {
    fn create_run(&self, _date: &str, slug: &str) -> Result<PathBuf, Error> {
        Ok(PathBuf::from(format!("/tmp/run/{slug}")))
    }

    fn read_artifact(&self, _run: &Path, name: &str) -> Result<String, Error> {
        self.files
            .lock()
            .expect("store mutex")
            .get(name)
            .cloned()
            .ok_or_else(|| Error::from(ArtifactError::artifact(format!("no artifact {name}"))))
    }

    fn write_artifact(&self, _run: &Path, name: &str, content: &str) -> Result<(), Error> {
        self.files
            .lock()
            .expect("store mutex")
            .insert(name.to_string(), content.to_string());
        Ok(())
    }
}

/// Persists the action log and status snapshot for a run.
pub trait StateStore: Send + Sync {
    /// Append one action-log event.
    fn append_action(&self, run: &Path, event: &ActionEvent) -> Result<(), Error>;

    /// Read all action-log events, oldest first.
    fn read_actions(&self, run: &Path) -> Result<Vec<ActionEvent>, Error>;

    /// Write the status snapshot.
    fn write_snapshot(&self, run: &Path, snapshot: &StatusSnapshot) -> Result<(), Error>;

    /// Read the status snapshot. Returns an error when no snapshot exists.
    fn read_snapshot(&self, run: &Path) -> Result<StatusSnapshot, Error>;
}

/// Test double for [`StateStore`]: keeps events and the latest snapshot in memory.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MemoryStateStore {
    events: std::sync::Mutex<Vec<ActionEvent>>,
    snapshot: std::sync::Mutex<Option<StatusSnapshot>>,
}

#[cfg(test)]
impl MemoryStateStore {
    /// Recorded action-log events, in order.
    #[must_use]
    pub fn events(&self) -> Vec<ActionEvent> {
        self.events.lock().expect("store mutex").clone()
    }
}

#[cfg(test)]
impl StateStore for MemoryStateStore {
    fn append_action(&self, _run: &Path, event: &ActionEvent) -> Result<(), Error> {
        self.events.lock().expect("store mutex").push(event.clone());
        Ok(())
    }

    fn read_actions(&self, _run: &Path) -> Result<Vec<ActionEvent>, Error> {
        Ok(self.events())
    }

    fn write_snapshot(&self, _run: &Path, snapshot: &StatusSnapshot) -> Result<(), Error> {
        *self.snapshot.lock().expect("store mutex") = Some(snapshot.clone());
        Ok(())
    }

    fn read_snapshot(&self, _run: &Path) -> Result<StatusSnapshot, Error> {
        self.snapshot
            .lock()
            .expect("store mutex")
            .clone()
            .ok_or_else(|| Error::from(ArtifactError::artifact("no snapshot written")))
    }
}

/// Builds a [`ModelClient`] for a role's provider, spec, and options.
///
/// The application depends on this factory port rather than on any concrete
/// provider, so the orchestrator can select a per-role client without knowing
/// about adapters. The adapters layer implements it.
pub trait ModelClientFactory: Send + Sync {
    /// Build a client for `provider`, honoring `spec` (base URL / key env) and
    /// `options` (temperature, max_tokens, think, …).
    fn build(
        &self,
        provider: &str,
        spec: Option<&ProviderSpec>,
        options: &BTreeMap<String, toml::Value>,
    ) -> Result<Box<dyn ModelClient>, Error>;
}

/// Runs a declared capability (`run-tests`, `apply-patch`, `write-file`,
/// `search-replace`, `gather-context`).
#[async_trait::async_trait]
pub trait ToolRunner: Send + Sync {
    /// Run `tool` against `repo`, using `input` (the role's artifact text, or
    /// empty when the tool needs no input). Returns the tool's output.
    async fn run(&self, tool: &str, repo: &Path, input: &str) -> Result<String, Error>;
}

/// Gathers repo context (codegraph/graphify) for context injection.
#[async_trait::async_trait]
pub trait ContextProvider: Send + Sync {
    /// Gather context about `repo` for `goal`, returning text to inject.
    async fn gather(&self, repo: &Path, goal: &str) -> Result<String, Error>;
}

/// Test double for [`ToolRunner`]: returns a fixed reply for any tool.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct StubToolRunner {
    output: std::sync::Mutex<String>,
    calls: std::sync::Mutex<Vec<(String, String)>>,
}

#[cfg(test)]
impl StubToolRunner {
    /// A stub that always returns `output`.
    #[must_use]
    pub fn new(output: impl Into<String>) -> Self {
        Self {
            output: std::sync::Mutex::new(output.into()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Recorded `(tool, input)` pairs, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("stub mutex").clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl ToolRunner for StubToolRunner {
    async fn run(&self, tool: &str, _repo: &Path, input: &str) -> Result<String, Error> {
        self.calls
            .lock()
            .expect("stub mutex")
            .push((tool.to_string(), input.to_string()));
        Ok(self.output.lock().expect("stub mutex").clone())
    }
}

/// Test double for [`ContextProvider`]: returns fixed text.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct StubContextProvider {
    text: String,
}

#[cfg(test)]
impl StubContextProvider {
    /// A stub that always returns `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl ContextProvider for StubContextProvider {
    async fn gather(&self, _repo: &Path, _goal: &str) -> Result<String, Error> {
        Ok(self.text.clone())
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
        client.push_err(Error::from(ProviderError::Timeout));
        let err = client
            .generate(&request())
            .await
            .expect_err("scripted error");
        assert!(matches!(err, Error::Provider(ProviderError::Timeout)));
    }

    #[tokio::test]
    async fn scripted_client_exhausted_replies_errors() {
        let client = ScriptedModelClient::new();
        let err = client.generate(&request()).await.expect_err("exhausted");
        assert!(err.to_string().contains("no scripted replies"), "{err}");
    }

    #[test]
    fn fixed_clock_returns_fixed_values() {
        let clock = FixedClock {
            now_iso: "2026-08-19T12:00:00Z".into(),
            today: "2026-08-19".into(),
        };
        assert_eq!(clock.now_iso(), "2026-08-19T12:00:00Z");
        assert_eq!(clock.today(), "2026-08-19");
    }

    #[test]
    fn memory_artifact_store_round_trips() {
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        store
            .write_artifact(run, "01_brief.md", "goal")
            .expect("write");
        assert_eq!(
            store.read_artifact(run, "01_brief.md").expect("read"),
            "goal"
        );
    }

    #[test]
    fn memory_state_store_round_trips_snapshot() {
        use crate::domain::rolegraph::state::RunStatus;
        use crate::domain::rolegraph::verdict::BlockReason;
        let store = MemoryStateStore::default();
        let run = Path::new("/tmp/run/x");
        let snap = StatusSnapshot {
            current_role: Some("qa".into()),
            steps: 3,
            last_verdict: None,
            status: RunStatus::Running,
            block_reason: BlockReason::None,
            loop_counters: std::collections::BTreeMap::new(),
        };
        store.write_snapshot(run, &snap).expect("write");
        assert_eq!(store.read_snapshot(run).expect("read"), snap);
    }

    #[tokio::test]
    async fn stub_tool_runner_returns_fixed_output() {
        let runner = StubToolRunner::new("all tests pass");
        let out = runner
            .run("run-tests", Path::new("/repo"), "")
            .await
            .expect("run");
        assert_eq!(out, "all tests pass");
    }

    #[tokio::test]
    async fn stub_context_provider_returns_fixed_text() {
        let provider = StubContextProvider::new("graph context");
        let text = provider
            .gather(Path::new("/repo"), "goal")
            .await
            .expect("gather");
        assert_eq!(text, "graph context");
    }
}
