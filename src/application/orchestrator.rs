//! Graph orchestrator: walk the role graph, routing on verdicts.

use crate::application::executor::{self, RoleContext};
use crate::domain::rolegraph::config::{NightshiftConfig, OnUnclear, RoleSpec};
use crate::domain::rolegraph::routing::Target;
use crate::domain::rolegraph::state::{
    ActionEvent, EventKind, ModelCall, RunIdentity, RunStatus, StatusSnapshot,
};
use crate::domain::rolegraph::verdict::{BlockReason, Verdict};
use crate::error::Error;
use crate::ports::{
    ArtifactStore, Clock, ContextProvider, ModelClient, ModelClientFactory, StateStore, ToolRunner,
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
    /// The config and target-repo state this run starts with. Every checkpoint
    /// records it, and a resume must match it unless `force_resume` is set.
    pub identity: RunIdentity,
    /// The checkpoint to continue from (`harness --resume`); `None` starts fresh.
    pub resume: Option<StatusSnapshot>,
    /// Continue a resume even though the identity no longer matches.
    pub force_resume: bool,
}

/// What a checkpoint records: the state the next role starts with.
struct Progress {
    /// Role executions so far.
    steps: u32,
    /// The last verdict recorded.
    last_verdict: Option<Verdict>,
    /// Back-edge counts, keyed by `"role:target"`.
    loop_counters: BTreeMap<String, u32>,
    /// Role output files written so far.
    artifacts: Vec<String>,
    /// The goal and the loop-back context for the next role.
    context: RoleContext,
    /// The config and target-repo state the run started with.
    identity: RunIdentity,
}

impl Progress {
    fn new(goal: &str, identity: &RunIdentity) -> Self {
        Self {
            steps: 0,
            last_verdict: None,
            loop_counters: BTreeMap::new(),
            artifacts: Vec::new(),
            context: RoleContext {
                goal: goal.to_string(),
                findings: Vec::new(),
                questions: Vec::new(),
                clarifications: Vec::new(),
            },
            identity: identity.clone(),
        }
    }

    /// The snapshot that records `current` with `status`.
    fn snapshot(
        &self,
        current: &str,
        status: RunStatus,
        block_reason: BlockReason,
    ) -> StatusSnapshot {
        StatusSnapshot {
            current_role: Some(current.to_string()),
            steps: self.steps,
            last_verdict: self.last_verdict,
            status,
            block_reason,
            config_hash: self.identity.config_hash.clone(),
            repo_head: self.identity.repo_head.clone(),
            loop_counters: self.loop_counters.clone(),
            goal: self.context.goal.clone(),
            artifacts: self.artifacts.clone(),
            findings: self.context.findings.clone(),
            questions: self.context.questions.clone(),
        }
    }
}

/// Walk the role graph from `request.config.run.start`, routing deterministically
/// on each role's verdict until a terminal state is reached.
///
/// Before each role starts, a `running` checkpoint is written to the state
/// store, so a crash during the role resumes from that role (see
/// [`RunRequest::resume`]). A resumed run appends a `resumed` event.
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
    let mut progress = Progress::new(request.goal, &request.identity);
    if let Some(checkpoint) = &request.resume {
        if checkpoint.status != RunStatus::Running {
            // A finished run has nothing left to do: report its status, call no model.
            return Ok(RunResult {
                status: checkpoint.status,
                block_reason: checkpoint.block_reason,
                steps: checkpoint.steps,
            });
        }
        let mismatches = checkpoint.identity().mismatches(&request.identity);
        if !mismatches.is_empty() && !request.force_resume {
            return Err(Error::Resume(format!(
                "{}: {}; pass --force-resume to continue anyway",
                request.run.display(),
                mismatches.join("; ")
            )));
        }
        current = checkpoint.current_role.clone().ok_or_else(|| {
            Error::Resume(format!(
                "{}: state.json names no role to resume at",
                request.run.display()
            ))
        })?;
        progress.steps = checkpoint.steps;
        progress.last_verdict = checkpoint.last_verdict;
        progress.loop_counters = checkpoint.loop_counters.clone();
        progress.artifacts = checkpoint.artifacts.clone();
        progress.context.findings = checkpoint.findings.clone();
        progress.context.questions = checkpoint.questions.clone();
        let detail = if mismatches.is_empty() {
            format!("resumed at step {}", progress.steps)
        } else {
            format!(
                "resumed at step {}, forcing past: {}",
                progress.steps,
                mismatches.join("; ")
            )
        };
        state.append_action(
            request.run,
            &ActionEvent {
                ts: clock.now_iso(),
                event: EventKind::Resumed,
                role: current.clone(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: Some(detail),
                call: None,
            },
        )?;
    }

    loop {
        if progress.steps >= request.config.run.max_steps {
            return halt_budget_exhausted(state, clock, request.run, &current, &progress);
        }

        let role = roles
            .get(current.as_str())
            .copied()
            .ok_or_else(|| Error::RoleGraph(format!("routing reached unknown role {current:?}")))?;

        // The checkpoint is what this role starts with: a crash during it resumes here.
        state.write_snapshot(
            request.run,
            &progress.snapshot(&current, RunStatus::Running, BlockReason::None),
        )?;
        progress.steps += 1;

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
                detail: None,
                call: None,
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
                    &progress,
                )?;
                return Err(error);
            }
        };
        let on_retry = |attempt: u32, error: &Error| {
            state.append_action(
                request.run,
                &ActionEvent {
                    ts: clock.now_iso(),
                    event: EventKind::Retry,
                    role: current.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    verdict: None,
                    artifact: None,
                    block_reason: BlockReason::None,
                    detail: Some(format!("attempt {attempt} rejected: {error}")),
                    call: None,
                },
            )
        };
        let log_call = |call: ModelCall| -> Result<(), Error> {
            state.append_action(
                request.run,
                &ActionEvent {
                    ts: clock.now_iso(),
                    event: EventKind::LlmCall,
                    role: current.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    verdict: None,
                    artifact: None,
                    block_reason: BlockReason::None,
                    detail: None,
                    call: Some(call),
                },
            )
        };
        let metered = Metered {
            inner: client.as_ref(),
            log: log_call,
        };
        let outcome = match executor::execute(
            &metered,
            store,
            tools,
            context_provider,
            &executor::ExecuteParams {
                run: request.run,
                repo: request.repo,
                role,
                context: &progress.context,
                artifacts: &progress.artifacts,
                on_retry: &on_retry,
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
                    &progress,
                )?;
                return Err(error);
            }
        };
        progress.last_verdict = Some(outcome.output.verdict);
        if let Some(name) = &outcome.artifact {
            if !progress.artifacts.contains(name) {
                progress.artifacts.push(name.clone());
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
                detail: None,
                call: None,
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
                        detail: None,
                        call: None,
                    },
                )?;
                state.write_snapshot(request.run, &progress.snapshot(&current, status, reason))?;
                return Ok(RunResult {
                    status,
                    block_reason: reason,
                    steps: progress.steps,
                });
            }
            RoutingDecision::Next(next) => {
                current = next;
                progress.context = RoleContext {
                    goal: request.goal.to_string(),
                    findings: Vec::new(),
                    questions: Vec::new(),
                    clarifications: Vec::new(),
                };
            }
            RoutingDecision::LoopBack(target, findings, questions) => {
                let key = format!("{current}:{target}");
                let count = progress.loop_counters.get(&key).copied().unwrap_or(0);
                if count >= role.max_loop {
                    return halt_budget_exhausted(state, clock, request.run, &current, &progress);
                }
                progress.loop_counters.insert(key, count + 1);
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
                        detail: None,
                        call: None,
                    },
                )?;
                progress.context = RoleContext {
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
    progress: &Progress,
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
            detail: None,
            call: None,
        },
    )?;
    state.write_snapshot(run, &progress.snapshot(current, RunStatus::Blocked, reason))?;
    Ok(RunResult {
        status: RunStatus::Blocked,
        block_reason: reason,
        steps: progress.steps,
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
    progress: &Progress,
) -> Result<(), Error> {
    let reason = BlockReason::ToolFailure;
    state.append_action(
        run,
        &ActionEvent {
            ts: clock.now_iso(),
            event: EventKind::Fail,
            role: ctx.current.clone(),
            provider: ctx.provider.clone(),
            model: ctx.model.clone(),
            verdict: None,
            artifact: None,
            block_reason: reason,
            detail: None,
            call: None,
        },
    )?;
    state.write_snapshot(
        run,
        &progress.snapshot(&ctx.current, RunStatus::Failed, reason),
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

/// Wraps the role's [`ModelClient`]: times each call, failed ones too, and passes
/// the time and the token counts the server reported to `log` (one `llm_call` event).
struct Metered<'a, L> {
    inner: &'a dyn ModelClient,
    log: L,
}

#[async_trait::async_trait]
impl<L> ModelClient for Metered<'_, L>
where
    L: Fn(ModelCall) -> Result<(), Error> + Send + Sync,
{
    async fn generate(&self, request: &crate::ports::GenerateRequest) -> Result<String, Error> {
        Ok(self.generate_with_usage(request).await?.0)
    }

    async fn generate_with_usage(
        &self,
        request: &crate::ports::GenerateRequest,
    ) -> Result<(String, Option<crate::ports::Usage>), Error> {
        let started = std::time::Instant::now();
        let reply = self.inner.generate_with_usage(request).await;
        let usage = reply.as_ref().ok().and_then(|(_, usage)| *usage);
        let logged = (self.log)(ModelCall {
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            prompt_tokens: usage.map(|u| u.prompt_tokens),
            completion_tokens: usage.map(|u| u.completion_tokens),
        });
        // The model's error comes first; a failed log write fails a call that succeeded.
        let reply = reply?;
        logged?;
        Ok(reply)
    }

    fn redacted_origin(&self) -> Option<String> {
        self.inner.redacted_origin()
    }
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
                identity: RunIdentity::default(),
                resume: None,
                force_resume: false,
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
                identity: RunIdentity::default(),
                resume: None,
                force_resume: false,
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
    #[tokio::test]
    async fn rejected_reply_is_retried_and_logged() {
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
        let factory = QueueFactory::new();
        factory.push("not json");
        factory.push(r#"{"verdict":"done","content":""}"#);
        let state = MemoryStateStore::default();
        let result = run_graph(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &clock(),
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &RunRequest {
                run: Path::new("/tmp/run/x"),
                repo: Path::new("/repo"),
                config: &cfg,
                goal: "goal",
                identity: RunIdentity::default(),
                resume: None,
                force_resume: false,
            },
        )
        .await
        .expect("run");
        assert_eq!(result.status, RunStatus::Done);
        assert_eq!(result.steps, 1, "a retry is the same step");
        let events = state.events();
        let retries: Vec<_> = events
            .iter()
            .filter(|e| e.event == EventKind::Retry)
            .collect();
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].role, "po");
        let detail = retries[0].detail.as_deref().expect("detail");
        assert!(detail.starts_with("attempt 1 rejected: "), "{detail}");
        assert!(detail.contains("JSON"), "{detail}");
    }

    /// Replies with fixed token counts, or fails when `fail` is set.
    struct UsageClient {
        usage: Option<crate::ports::Usage>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl crate::ports::ModelClient for UsageClient {
        async fn generate(&self, request: &crate::ports::GenerateRequest) -> Result<String, Error> {
            Ok(self.generate_with_usage(request).await?.0)
        }

        async fn generate_with_usage(
            &self,
            _request: &crate::ports::GenerateRequest,
        ) -> Result<(String, Option<crate::ports::Usage>), Error> {
            if self.fail {
                return Err(Error::Context("server down".into()));
            }
            Ok(("reply".into(), self.usage))
        }
    }

    #[tokio::test]
    async fn metered_logs_token_counts_and_failed_calls() {
        let logged = Mutex::new(Vec::new());
        let log = |call: ModelCall| -> Result<(), Error> {
            logged.lock().expect("log").push(call);
            Ok(())
        };
        let request = crate::ports::GenerateRequest {
            model: "m".into(),
            system: None,
            prompt: "p".into(),
            temperature: 0.2,
            timeout: None,
        };
        let reporting = UsageClient {
            usage: Some(crate::ports::Usage {
                prompt_tokens: 12,
                completion_tokens: 3,
            }),
            fail: false,
        };
        let failing = UsageClient {
            usage: None,
            fail: true,
        };

        let reply = Metered {
            inner: &reporting,
            log: &log,
        }
        .generate(&request)
        .await;
        assert_eq!(reply.expect("reply"), "reply");
        let reply = Metered {
            inner: &failing,
            log: &log,
        }
        .generate(&request)
        .await;
        assert!(reply
            .expect_err("fails")
            .to_string()
            .contains("server down"));

        let logged = logged.into_inner().expect("log");
        let tokens: Vec<_> = logged
            .iter()
            .map(|c| (c.prompt_tokens, c.completion_tokens))
            .collect();
        assert_eq!(tokens, [(Some(12), Some(3)), (None, None)]);
    }

    #[tokio::test]
    async fn llm_call_events_are_logged() {
        let cfg = config(TWO_ROLE);
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"continue","content":"brief"}"#);
        factory.push(r#"{"verdict":"done","content":"patch"}"#);
        let state = MemoryStateStore::default();
        let _result = run_graph(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &clock(),
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &RunRequest {
                run: Path::new("/tmp/run/x"),
                repo: Path::new("/repo"),
                config: &cfg,
                goal: "goal",
                identity: RunIdentity::default(),
                resume: None,
                force_resume: false,
            },
        )
        .await
        .expect("run");
        let events = state.events();
        let llm_calls: Vec<_> = events
            .iter()
            .filter(|e| e.event == EventKind::LlmCall)
            .collect();
        let roles: Vec<&str> = llm_calls.iter().map(|e| e.role.as_str()).collect();
        assert_eq!(roles, ["po", "dev"]);
        // QueueClient reports no usage, so each call has its time and no token counts.
        for event in llm_calls {
            let call = event.call.expect("llm_call has call");
            assert_eq!(call.prompt_tokens, None);
            assert_eq!(call.completion_tokens, None);
        }
    }

    /// Three roles: `po` and `dev` write outputs, `qa` loops back to `dev` on issues.
    const THREE_ROLE: &str = r#"
[run]
start = "po"

[[roles]]
id = "po"
provider = "ollama"
model = "phi4"
output = "01_brief.md"
on = { continue = "dev" }

[[roles]]
id = "dev"
provider = "ollama"
model = "phi4"
output = "02_patch.md"
on = { continue = "qa" }

[[roles]]
id = "qa"
provider = "ollama"
model = "phi4"
on = { issues = "dev" }
"#;

    /// Stops a run the way a crash would: appending `role`'s `role_start` fails
    /// before anything is recorded.
    struct CrashBefore<'a> {
        inner: &'a MemoryStateStore,
        role: &'a str,
    }

    impl StateStore for CrashBefore<'_> {
        fn append_action(&self, run: &Path, event: &ActionEvent) -> Result<(), Error> {
            if event.event == EventKind::RoleStart && event.role == self.role {
                return Err(Error::from(ArtifactError::artifact("simulated crash")));
            }
            self.inner.append_action(run, event)
        }

        fn read_actions(&self, run: &Path) -> Result<Vec<ActionEvent>, Error> {
            self.inner.read_actions(run)
        }

        fn write_snapshot(&self, run: &Path, snapshot: &StatusSnapshot) -> Result<(), Error> {
            self.inner.write_snapshot(run, snapshot)
        }

        fn read_snapshot(&self, run: &Path) -> Result<StatusSnapshot, Error> {
            self.inner.read_snapshot(run)
        }
    }

    fn identity(hash: &str, head: Option<&str>) -> RunIdentity {
        RunIdentity {
            config_hash: hash.into(),
            repo_head: head.map(str::to_string),
        }
    }

    /// A checkpoint as the orchestrator writes it before `role` starts.
    fn snapshot_at(role: &str, status: RunStatus, steps: u32, id: &RunIdentity) -> StatusSnapshot {
        StatusSnapshot {
            current_role: Some(role.into()),
            steps,
            last_verdict: Some(Verdict::Continue),
            status,
            block_reason: BlockReason::None,
            config_hash: id.config_hash.clone(),
            repo_head: id.repo_head.clone(),
            loop_counters: BTreeMap::new(),
            goal: "goal".into(),
            artifacts: Vec::new(),
            findings: Vec::new(),
            questions: Vec::new(),
        }
    }

    async fn drive<S: StateStore>(
        factory: &QueueFactory,
        artifacts: &MemoryArtifactStore,
        state: &S,
        cfg: &NightshiftConfig,
        identity: RunIdentity,
        resume: Option<StatusSnapshot>,
        force_resume: bool,
    ) -> Result<RunResult, Error> {
        run_graph(
            factory,
            artifacts,
            state,
            &clock(),
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &RunRequest {
                run: Path::new("/tmp/run/x"),
                repo: Path::new("/repo"),
                config: cfg,
                goal: "goal",
                identity,
                resume,
                force_resume,
            },
        )
        .await
    }

    #[tokio::test]
    async fn resume_runs_only_the_checkpointed_role_and_continues_the_step_count() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"continue","content":"brief"}"#);
        factory.push(r#"{"verdict":"continue","content":"patch"}"#);
        factory.push(r#"{"verdict":"done","content":""}"#);
        let artifacts = MemoryArtifactStore::default();
        let state = MemoryStateStore::default();
        let id = identity("c1", Some("abc"));

        // The process dies just before qa starts; po and dev have finished.
        let crashed = drive(
            &factory,
            &artifacts,
            &CrashBefore {
                inner: &state,
                role: "qa",
            },
            &cfg,
            id.clone(),
            None,
            false,
        )
        .await;
        assert!(crashed.is_err());
        let checkpoint = state
            .read_snapshot(Path::new("/tmp/run/x"))
            .expect("checkpoint");
        assert_eq!(checkpoint.current_role.as_deref(), Some("qa"));
        assert_eq!(checkpoint.steps, 2);
        assert_eq!(checkpoint.status, RunStatus::Running);
        let before = state.events().len();

        let resumed = drive(
            &factory,
            &artifacts,
            &state,
            &cfg,
            id,
            Some(checkpoint),
            false,
        )
        .await
        .expect("resume");

        assert_eq!(resumed.status, RunStatus::Done);
        assert_eq!(resumed.steps, 3);
        let events = state.events();
        let new_events = &events[before..];
        let started: Vec<&str> = new_events
            .iter()
            .filter(|e| e.event == EventKind::RoleStart)
            .map(|e| e.role.as_str())
            .collect();
        assert_eq!(started, ["qa"]);
        let resumed_event = new_events
            .iter()
            .find(|e| e.event == EventKind::Resumed)
            .expect("resumed event");
        assert_eq!(resumed_event.role, "qa");
        assert_eq!(resumed_event.detail.as_deref(), Some("resumed at step 2"));
        let finished = state.read_snapshot(Path::new("/tmp/run/x")).expect("final");
        assert_eq!(finished.artifacts, ["01_brief.md", "02_patch.md"]);
    }

    #[tokio::test]
    async fn resume_refuses_a_changed_config_and_names_the_mismatch() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        let state = MemoryStateStore::default();
        let recorded = identity("c1", Some("abc"));
        let checkpoint = snapshot_at("qa", RunStatus::Running, 2, &recorded);

        let error = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &cfg,
            identity("c2", Some("abc")),
            Some(checkpoint),
            false,
        )
        .await
        .expect_err("config changed");

        let message = error.to_string();
        assert!(message.contains("config changed"), "{message}");
        assert!(
            message.contains("c1") && message.contains("c2"),
            "{message}"
        );
        assert!(message.contains("--force-resume"), "{message}");
        assert!(
            state.events().is_empty(),
            "refused before recording anything"
        );
    }

    #[tokio::test]
    async fn resume_refuses_a_moved_repo_head_and_names_both_commits() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        let state = MemoryStateStore::default();
        let checkpoint = snapshot_at("qa", RunStatus::Running, 2, &identity("c1", Some("abc")));

        let error = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &cfg,
            identity("c1", Some("def")),
            Some(checkpoint),
            false,
        )
        .await
        .expect_err("head moved");

        let message = error.to_string();
        assert!(message.contains("HEAD changed"), "{message}");
        assert!(
            message.contains("abc") && message.contains("def"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn resume_with_force_continues_past_a_mismatch_and_records_the_new_identity() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"done","content":""}"#);
        let state = MemoryStateStore::default();
        let checkpoint = snapshot_at("qa", RunStatus::Running, 2, &identity("c1", Some("abc")));

        let resumed = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &cfg,
            identity("c2", Some("abc")),
            Some(checkpoint),
            true,
        )
        .await
        .expect("forced resume");

        assert_eq!(resumed.status, RunStatus::Done);
        let event = state
            .events()
            .into_iter()
            .find(|e| e.event == EventKind::Resumed)
            .expect("resumed event");
        assert!(
            event.detail.unwrap_or_default().contains("forcing past"),
            "the forced mismatch is recorded"
        );
        let finished = state.read_snapshot(Path::new("/tmp/run/x")).expect("final");
        assert_eq!(finished.config_hash, "c2");
    }

    #[tokio::test]
    async fn resuming_a_finished_run_returns_its_status_without_calling_the_model() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"done","content":""}"#);
        let state = MemoryStateStore::default();
        let id = identity("c1", Some("abc"));
        let finished = snapshot_at("qa", RunStatus::Done, 4, &id);

        let result = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &cfg,
            id,
            Some(finished),
            false,
        )
        .await
        .expect("finished run");

        assert_eq!(
            result,
            RunResult {
                status: RunStatus::Done,
                block_reason: BlockReason::None,
                steps: 4,
            }
        );
        assert_eq!(
            factory.replies.lock().expect("queue").len(),
            1,
            "no reply was consumed"
        );
        assert!(state.events().is_empty());
    }

    #[tokio::test]
    async fn resume_keeps_loop_counters_so_the_loop_cap_still_holds() {
        let cfg = config(THREE_ROLE);
        let factory = QueueFactory::new();
        factory.push(r#"{"verdict":"issues","findings":["still broken"],"content":""}"#);
        let state = MemoryStateStore::default();
        let id = identity("c1", Some("abc"));
        let mut checkpoint = snapshot_at("qa", RunStatus::Running, 2, &id);
        // dev -> qa -> dev has already used its full max_loop (3) before the crash.
        checkpoint.loop_counters = BTreeMap::from([("qa:dev".to_string(), 3)]);

        let resumed = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &state,
            &cfg,
            id,
            Some(checkpoint),
            false,
        )
        .await
        .expect("resume");

        assert_eq!(resumed.status, RunStatus::Blocked);
        assert_eq!(resumed.block_reason, BlockReason::BudgetExhausted);
        assert_eq!(resumed.steps, 3);
    }

    #[tokio::test]
    async fn a_fresh_run_records_its_identity_from_the_first_checkpoint() {
        let cfg = config(TWO_ROLE);
        let factory = QueueFactory::new();
        let state = MemoryStateStore::default();
        let id = identity("c1", Some("abc"));

        // The first role crashes before it starts: the checkpoint already holds the identity.
        let crashed = drive(
            &factory,
            &MemoryArtifactStore::default(),
            &CrashBefore {
                inner: &state,
                role: "po",
            },
            &cfg,
            id.clone(),
            None,
            false,
        )
        .await;

        assert!(crashed.is_err());
        let checkpoint = state
            .read_snapshot(Path::new("/tmp/run/x"))
            .expect("checkpoint at run start");
        assert_eq!(checkpoint.identity(), id);
        assert_eq!(checkpoint.current_role.as_deref(), Some("po"));
        assert_eq!(checkpoint.goal, "goal");
    }
}
