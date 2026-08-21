//! Graph orchestrator: walk the role graph, routing on verdicts.

use crate::application::executor::{self, RoleContext};
use crate::domain::rolegraph::config::{NightshiftConfig, OnUnclear, RoleSpec};
use crate::domain::rolegraph::routing::Target;
use crate::domain::rolegraph::state::{ActionEvent, EventKind, RunStatus, StatusSnapshot};
use crate::domain::rolegraph::verdict::{BlockReason, Verdict};
use crate::error::Error;
use crate::ports::{
    ArtifactStore, Clock, ContextProvider, ModelClientFactory, StateStore, ToolRunner,
};
use std::collections::BTreeMap;
use std::path::Path;

/// Result of walking the graph to a terminal state.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// How the run ended.
    pub status: RunStatus,
    /// Why, when the run did not finish `done`.
    pub block_reason: BlockReason,
    /// Total role executions performed.
    pub steps: u32,
}

/// Per-run inputs (borrowed data; ports are passed separately).
pub struct RunRequest<'a> {
    /// Run directory (artifacts + state).
    pub run: &'a Path,
    /// Target repo (for capabilities).
    pub repo: &'a Path,
    /// The role graph.
    pub config: &'a NightshiftConfig,
    /// The operator's goal.
    pub goal: &'a str,
}

/// Walk the role graph from `request.config.run.start`, routing deterministically
/// on each role's verdict until a terminal state is reached.
pub async fn run_graph<F, A, S, K, T, P>(
    factory: &F,
    store: &A,
    state: &S,
    clock: &K,
    tools: &T,
    context_provider: &P,
    request: &RunRequest<'_>,
) -> Result<RunResult, Error>
where
    F: ModelClientFactory,
    A: ArtifactStore,
    S: StateStore,
    K: Clock,
    T: ToolRunner,
    P: ContextProvider,
{
    let roles: BTreeMap<&str, &RoleSpec> = request
        .config
        .roles
        .iter()
        .map(|role| (role.id.as_str(), role))
        .collect();

    let mut current: String = request.config.run.start.clone();
    let mut steps: u32 = 0;
    let mut loop_counters: BTreeMap<String, u32> = BTreeMap::new();
    let mut last_verdict: Option<Verdict> = None;
    let mut context = RoleContext {
        goal: request.goal.to_string(),
        findings: Vec::new(),
        questions: Vec::new(),
        clarifications: Vec::new(),
    };
    let mut artifacts: Vec<String> = Vec::new();

    loop {
        if steps >= request.config.run.max_steps {
            return halt_budget_exhausted(
                state,
                clock,
                request.run,
                &current,
                steps,
                last_verdict,
                &loop_counters,
            );
        }

        let role = roles
            .get(current.as_str())
            .copied()
            .ok_or_else(|| Error::RoleGraph(format!("routing reached unknown role {current:?}")))?;

        steps += 1;

        let provider = role.provider.clone();
        let model = role.model.clone();
        state.append_action(
            request.run,
            &ActionEvent {
                ts: clock.now_iso(),
                event: EventKind::RoleStart,
                role: current.clone(),
                provider: provider.clone(),
                model: model.clone(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
            },
        )?;

        let spec = request.config.providers.get(&role.provider);
        let client = match factory.build(&role.provider, spec, &role.options) {
            Ok(c) => c,
            Err(error) => {
                record_failure(
                    state,
                    clock,
                    request.run,
                    &FailureCtx {
                        current: current.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                    },
                    steps,
                    &loop_counters,
                )?;
                return Err(error);
            }
        };
        let outcome = match executor::execute(
            client.as_ref(),
            store,
            tools,
            context_provider,
            &executor::ExecuteParams {
                run: request.run,
                repo: request.repo,
                role,
                context: &context,
                artifacts: &artifacts,
            },
        )
        .await
        {
            Ok(o) => o,
            Err(error) => {
                record_failure(
                    state,
                    clock,
                    request.run,
                    &FailureCtx {
                        current: current.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                    },
                    steps,
                    &loop_counters,
                )?;
                return Err(error);
            }
        };
        last_verdict = Some(outcome.output.verdict);
        if let Some(name) = &outcome.artifact {
            if !artifacts.contains(name) {
                artifacts.push(name.clone());
            }
        }

        state.append_action(
            request.run,
            &ActionEvent {
                ts: clock.now_iso(),
                event: EventKind::RoleEnd,
                role: current.clone(),
                provider: provider.clone(),
                model: model.clone(),
                verdict: Some(outcome.output.verdict),
                artifact: outcome.artifact.clone(),
                block_reason: outcome.output.block_reason,
            },
        )?;
        state.write_snapshot(
            request.run,
            &StatusSnapshot {
                current_role: Some(current.clone()),
                steps,
                last_verdict,
                status: RunStatus::Running,
                block_reason: BlockReason::None,
                loop_counters: loop_counters.clone(),
            },
        )?;

        match route(request.config, role, &outcome) {
            RoutingDecision::Terminal(status, reason) => {
                let reason = normalize_reason(status, reason);
                state.append_action(
                    request.run,
                    &ActionEvent {
                        ts: clock.now_iso(),
                        event: terminal_event(status),
                        role: current.clone(),
                        provider,
                        model,
                        verdict: Some(outcome.output.verdict),
                        artifact: outcome.artifact,
                        block_reason: reason,
                    },
                )?;
                state.write_snapshot(
                    request.run,
                    &StatusSnapshot {
                        current_role: Some(current.clone()),
                        steps,
                        last_verdict,
                        status,
                        block_reason: reason,
                        loop_counters: loop_counters.clone(),
                    },
                )?;
                return Ok(RunResult {
                    status,
                    block_reason: reason,
                    steps,
                });
            }
            RoutingDecision::Next(next) => {
                current = next;
                context = RoleContext {
                    goal: request.goal.to_string(),
                    findings: Vec::new(),
                    questions: Vec::new(),
                    clarifications: Vec::new(),
                };
            }
            RoutingDecision::LoopBack(target, findings, questions) => {
                let key = format!("{current}:{target}");
                let count = loop_counters.get(&key).copied().unwrap_or(0);
                if count >= role.max_loop {
                    return halt_budget_exhausted(
                        state,
                        clock,
                        request.run,
                        &current,
                        steps,
                        last_verdict,
                        &loop_counters,
                    );
                }
                loop_counters.insert(key, count + 1);
                state.append_action(
                    request.run,
                    &ActionEvent {
                        ts: clock.now_iso(),
                        event: EventKind::Loop,
                        role: current.clone(),
                        provider: String::new(),
                        model: String::new(),
                        verdict: None,
                        artifact: None,
                        block_reason: BlockReason::None,
                    },
                )?;
                context = RoleContext {
                    goal: request.goal.to_string(),
                    findings,
                    questions,
                    clarifications: Vec::new(),
                };
                current = target;
            }
        }
    }
}

/// What the orchestrator should do next after a role's verdict.
enum RoutingDecision {
    /// Stop with a terminal status and block reason.
    Terminal(RunStatus, BlockReason),
    /// Advance to the named role (a forward step).
    Next(String),
    /// Loop back to `target`, carrying `findings`/`questions`.
    LoopBack(String, Vec<String>, Vec<String>),
}

/// Decide the next move for a role's verdict, honoring the config's routing and
/// `on_unclear` policy.
fn route(
    config: &NightshiftConfig,
    role: &RoleSpec,
    outcome: &executor::ExecuteOutcome,
) -> RoutingDecision {
    let output = &outcome.output;
    match output.verdict {
        Verdict::Done => RoutingDecision::Terminal(RunStatus::Done, BlockReason::None),
        Verdict::Fail => RoutingDecision::Terminal(RunStatus::Failed, output.block_reason),
        Verdict::Continue => match role.on.continue_target() {
            Target::Done => RoutingDecision::Terminal(RunStatus::Done, BlockReason::None),
            Target::Halt => RoutingDecision::Terminal(RunStatus::Blocked, BlockReason::None),
            Target::Role(next) => RoutingDecision::Next(next),
        },
        Verdict::Issues => match role.on.issues_target() {
            Target::Halt => RoutingDecision::Terminal(RunStatus::Blocked, BlockReason::None),
            Target::Done => RoutingDecision::Terminal(RunStatus::Done, BlockReason::None),
            Target::Role(target) => {
                RoutingDecision::LoopBack(target, output.findings.clone(), Vec::new())
            }
        },
        Verdict::Questions => {
            let has_blocking = output.questions.iter().any(|question| question.blocking);
            // Non-blocking questions are recorded as assumptions and the run
            // proceeds; blocking questions halt (or proceed, per `on_unclear`).
            if !has_blocking || config.run.on_unclear == OnUnclear::Proceed {
                return match role.on.continue_target() {
                    Target::Done => RoutingDecision::Terminal(RunStatus::Done, BlockReason::None),
                    Target::Halt => {
                        RoutingDecision::Terminal(RunStatus::Blocked, BlockReason::None)
                    }
                    Target::Role(next) => RoutingDecision::Next(next),
                };
            }
            let questions = output
                .questions
                .iter()
                .map(|question| question.text.clone())
                .collect();
            match role.on.questions_target() {
                Target::Halt => {
                    RoutingDecision::Terminal(RunStatus::Blocked, BlockReason::IllDefinedTask)
                }
                Target::Done => RoutingDecision::Terminal(RunStatus::Done, BlockReason::None),
                Target::Role(target) => RoutingDecision::LoopBack(target, Vec::new(), questions),
            }
        }
    }
}

/// Normalize a terminal block reason: `fail` with no explicit reason is a tool
/// failure; anything else passes through.
fn normalize_reason(status: RunStatus, reason: BlockReason) -> BlockReason {
    if status == RunStatus::Failed && reason == BlockReason::None {
        BlockReason::ToolFailure
    } else {
        reason
    }
}

fn terminal_event(status: RunStatus) -> EventKind {
    match status {
        RunStatus::Done => EventKind::Done,
        RunStatus::Failed => EventKind::Fail,
        RunStatus::Blocked | RunStatus::Running => EventKind::Halt,
    }
}

/// Halt the run because a budget (global steps or a per-edge loop cap) was
/// exhausted, recording a halt event and the final `blocked` snapshot.
fn halt_budget_exhausted<S: StateStore, K: Clock>(
    state: &S,
    clock: &K,
    run: &Path,
    current: &str,
    steps: u32,
    last_verdict: Option<Verdict>,
    loop_counters: &BTreeMap<String, u32>,
) -> Result<RunResult, Error> {
    let reason = BlockReason::BudgetExhausted;
    state.append_action(
        run,
        &ActionEvent {
            ts: clock.now_iso(),
            event: EventKind::Halt,
            role: current.to_string(),
            provider: String::new(),
            model: String::new(),
            verdict: None,
            artifact: None,
            block_reason: reason,
        },
    )?;
    state.write_snapshot(
        run,
        &StatusSnapshot {
            current_role: Some(current.to_string()),
            steps,
            last_verdict,
            status: RunStatus::Blocked,
            block_reason: reason,
            loop_counters: loop_counters.clone(),
        },
    )?;
    Ok(RunResult {
        status: RunStatus::Blocked,
        block_reason: reason,
        steps,
    })
}

/// Record a `Fail` action event and a `Failed` snapshot when `execute` or
/// `factory.build` returns an error, so the persisted state does not stay
/// stuck at `Running` after the process exits.
fn record_failure<S: StateStore, K: Clock>(
    state: &S,
    clock: &K,
    run: &Path,
    ctx: &FailureCtx,
    steps: u32,
    loop_counters: &BTreeMap<String, u32>,
) -> Result<(), Error> {
    let reason = BlockReason::ToolFailure;
    state.append_action(
        run,
        &ActionEvent {
            ts: clock.now_iso(),
            event: EventKind::Fail,
            role: ctx.current.to_string(),
            provider: ctx.provider.to_string(),
            model: ctx.model.to_string(),
            verdict: None,
            artifact: None,
            block_reason: reason,
        },
    )?;
    state.write_snapshot(
        run,
        &StatusSnapshot {
            current_role: Some(ctx.current.to_string()),
            steps,
            last_verdict: None,
            status: RunStatus::Failed,
            block_reason: reason,
            loop_counters: loop_counters.clone(),
        },
    )?;
    Ok(())
}

/// Role context for [`record_failure`]: the active role id, provider, and model.
struct FailureCtx {
    /// Role id.
    current: String,
    /// Provider name.
    provider: String,
    /// Model tag.
    model: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ArtifactError;
    use crate::ports::{
        FixedClock, MemoryArtifactStore, MemoryStateStore, StubContextProvider, StubToolRunner,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Factory that returns clients sharing one scripted reply queue.
    struct QueueFactory {
        replies: Arc<Mutex<VecDeque<String>>>,
    }

    impl QueueFactory {
        fn new() -> Self {
            Self {
                replies: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        fn push(&self, text: &str) {
            self.replies.lock().expect("queue").push_back(text.into());
        }
    }

    impl ModelClientFactory for QueueFactory {
        fn build(
            &self,
            _provider: &str,
            _spec: Option<&crate::domain::rolegraph::config::ProviderSpec>,
            _options: &BTreeMap<String, toml::Value>,
        ) -> Result<Box<dyn crate::ports::ModelClient>, Error> {
            Ok(Box::new(QueueClient {
                replies: Arc::clone(&self.replies),
            }))
        }
    }

    struct QueueClient {
        replies: Arc<Mutex<VecDeque<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ModelClient for QueueClient {
        async fn generate(
            &self,
            _request: &crate::ports::GenerateRequest,
        ) -> Result<String, Error> {
            Ok(self
                .replies
                .lock()
                .expect("queue")
                .pop_front()
                .unwrap_or_else(|| r#"{"verdict":"done","content":""}"#.into()))
        }
    }

    fn clock() -> FixedClock {
        FixedClock {
            now_iso: "2026-08-20T00:00:00Z".into(),
            today: "2026-08-20".into(),
        }
    }

    fn config(toml: &str) -> NightshiftConfig {
        toml::from_str(toml).expect("config parses")
    }

    async fn run(cfg: &NightshiftConfig, factory: &QueueFactory) -> RunResult {
        let store = MemoryArtifactStore::default();
        let state = MemoryStateStore::default();
        run_graph(
            factory,
            &store,
            &state,
            &clock(),
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &RunRequest {
                run: Path::new("/tmp/run/x"),
                repo: Path::new("/repo"),
                config: cfg,
                goal: "goal",
            },
        )
        .await
        .expect("run")
    }

    const TWO_ROLE: &str = r#"
[run]
start = "po"

[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
on = { continue = "dev" }

[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
"#;

    #[tokio::test]
    async fn linear_graph_runs_to_done() {
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"continue","content":"brief"}"#);
        factory.push(r#"{"verdict":"done","content":"patch"}"#);
        let result = run(&config(TWO_ROLE), &factory).await;
        assert_eq!(result.status, RunStatus::Done);
        assert_eq!(result.block_reason, BlockReason::None);
        assert_eq!(result.steps, 2);
    }

    #[tokio::test]
    async fn issues_loop_caps_at_max_loop() {
        let factory = QueueFactory::new();
        for _ in 0..3 {
            factory.push(r#"{"verdict":"continue","content":""}"#);
            factory.push(r#"{"verdict":"issues","findings":["compile error"],"content":""}"#);
        }
        let cfg = config(
            r#"
[run]
start = "dev"

[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
on = { continue = "qa" }

[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
on = { issues = "dev" }
max_loop = 2
"#,
        );
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Blocked);
        assert_eq!(result.block_reason, BlockReason::BudgetExhausted);
        assert_eq!(result.steps, 6);
    }

    #[tokio::test]
    async fn blocking_questions_halt() {
        let factory = QueueFactory::new();
        factory.push(
            r#"{"verdict":"questions","questions":[{"text":"which port?","blocking":true}]}"#,
        );
        let cfg = config(
            r#"
[run]
start = "po"
[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
"#,
        );
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Blocked);
        assert_eq!(result.block_reason, BlockReason::IllDefinedTask);
    }

    #[tokio::test]
    async fn fail_is_terminal() {
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"fail","block_reason":"tool_failure"}"#);
        let cfg = config(
            r#"
[run]
start = "qa"
[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
"#,
        );
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Failed);
        assert_eq!(result.block_reason, BlockReason::ToolFailure);
    }

    #[tokio::test]
    async fn on_unclear_proceed_treats_questions_as_continue() {
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"questions","questions":[{"text":"q","blocking":true}]}"#);
        let cfg = config(
            r#"
[run]
start = "po"
on_unclear = "proceed"
[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
"#,
        );
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Done);
    }

    #[tokio::test]
    async fn non_blocking_questions_proceed() {
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"questions","questions":[{"text":"minor","blocking":false}]}"#);
        let cfg = config(
            r#"
[run]
start = "po"
[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
"#,
        );
        // Non-blocking questions are recorded and the run proceeds (the
        // continue target defaults to `done`).
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Done);
    }

    #[tokio::test]
    async fn max_steps_caps_a_self_loop() {
        let factory = QueueFactory::new();
        for _ in 0..4 {
            factory.push(r#"{"verdict":"continue","content":""}"#);
        }
        let cfg = config(
            r#"
[run]
start = "loop"
max_steps = 3
[[roles]]
id = "loop"
provider = "ollama"
model = "phi4"
on = { continue = "loop" }
"#,
        );
        let result = run(&cfg, &factory).await;
        assert_eq!(result.status, RunStatus::Blocked);
        assert_eq!(result.block_reason, BlockReason::BudgetExhausted);
        assert_eq!(result.steps, 3);
    }

    /// Factory whose `build` always fails — exercises the error path before
    /// the client is even constructed.
    struct ErrorFactory;

    impl ModelClientFactory for ErrorFactory {
        fn build(
            &self,
            _provider: &str,
            _spec: Option<&crate::domain::rolegraph::config::ProviderSpec>,
            _options: &BTreeMap<String, toml::Value>,
        ) -> Result<Box<dyn crate::ports::ModelClient>, Error> {
            Err(Error::from(ArtifactError::artifact("factory boom")))
        }
    }

    /// Run the graph and return the error result (not `RunResult`), so the
    /// caller can assert that the error was propagated and then inspect the
    /// state store for the `Failed` snapshot.
    async fn run_err<F: ModelClientFactory>(
        cfg: &NightshiftConfig,
        factory: &F,
        state: &MemoryStateStore,
    ) -> Error {
        let store = MemoryArtifactStore::default();
        run_graph(
            factory,
            &store,
            state,
            &clock(),
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &RunRequest {
                run: Path::new("/tmp/run/x"),
                repo: Path::new("/repo"),
                config: cfg,
                goal: "goal",
            },
        )
        .await
        .expect_err("should error")
    }

    #[tokio::test]
    async fn build_error_records_failed_snapshot() {
        let cfg = config(
            r#"
[run]
start = "po"
[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
"#,
        );
        let state = MemoryStateStore::default();
        let error = run_err(&cfg, &ErrorFactory, &state).await;
        assert!(error.to_string().contains("factory boom"), "{error}");
        let snap = state
            .read_snapshot(Path::new("/tmp/run/x"))
            .expect("snapshot");
        assert_eq!(snap.status, RunStatus::Failed, "snapshot should be Failed");
        assert_eq!(snap.block_reason, BlockReason::ToolFailure);
        assert_eq!(snap.current_role.as_deref(), Some("po"));
        let events = state.events();
        let fail = events.iter().find(|e| e.event == EventKind::Fail);
        assert!(fail.is_some(), "actions should contain a Fail event");
        assert_eq!(fail.unwrap().role, "po");
    }

    /// Client whose `generate` always fails — exercises the error path inside
    /// `executor::execute`.
    struct ErrorClient;

    #[async_trait::async_trait]
    impl crate::ports::ModelClient for ErrorClient {
        async fn generate(
            &self,
            _request: &crate::ports::GenerateRequest,
        ) -> Result<String, Error> {
            Err(Error::from(ArtifactError::artifact("generate boom")))
        }
    }

    /// Factory that always returns [`ErrorClient`].
    struct ErrorClientFactory;

    impl ModelClientFactory for ErrorClientFactory {
        fn build(
            &self,
            _provider: &str,
            _spec: Option<&crate::domain::rolegraph::config::ProviderSpec>,
            _options: &BTreeMap<String, toml::Value>,
        ) -> Result<Box<dyn crate::ports::ModelClient>, Error> {
            Ok(Box::new(ErrorClient))
        }
    }

    #[tokio::test]
    async fn execute_error_records_failed_snapshot() {
        let cfg = config(
            r#"
[run]
start = "po"
[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
"#,
        );
        let state = MemoryStateStore::default();
        let error = run_err(&cfg, &ErrorClientFactory, &state).await;
        assert!(error.to_string().contains("generate boom"), "{error}");
        let snap = state
            .read_snapshot(Path::new("/tmp/run/x"))
            .expect("snapshot");
        assert_eq!(snap.status, RunStatus::Failed, "snapshot should be Failed");
        assert_eq!(snap.block_reason, BlockReason::ToolFailure);
        assert_eq!(snap.current_role.as_deref(), Some("po"));
        let events = state.events();
        let fail = events.iter().find(|e| e.event == EventKind::Fail);
        assert!(fail.is_some(), "actions should contain a Fail event");
        assert_eq!(fail.unwrap().role, "po");
    }
}
