# src/application — the use cases

The application layer implements the role-graph use cases: pure orchestration
that depends only on `crate::domain` and `crate::ports` (ADR-007). It never
performs I/O and never imports an adapter — concrete implementations are
injected by the CLI edge (`main.rs`), which is also where every port is
satisfied.

| Module         | Use case |
| :------------- | :------- |
| `executor.rs`  | run one role: pre-tools (`gather-context`, `run-tests`) inject real results into the prompt; the LLM call goes through the `ModelClient` port; the verdict envelope is parsed; the deliverable is written via `ArtifactStore`; the post-tool `apply-patch` runs for `continue` / `done` verdicts |
| `orchestrator.rs` | walk the role graph from `run.start`: route deterministically on each verdict (`continue` / `issues` / `questions` / `done` / `fail`), enforce the global `max_steps` ceiling and per-role `max_loop` back-edge caps, append action-log events and the status snapshot via the `StateStore` / `Clock` ports, and stop at a terminal state |
| `report.rs`     | render the morning report from the persisted snapshot + action log: status label (`PASSED` / `FAILED` / `REQUIRES_HUMAN_REVIEW`), step count, block-reason description, and the role trail |

Both `executor::execute` and `orchestrator::run_graph` are generic over the
port traits (`ModelClient`, `ArtifactStore`, `StateStore`, `Clock`,
`ToolRunner`, `ContextProvider`, `ModelClientFactory`), so tests inject the
in-memory doubles from `ports.rs` and run with no network or filesystem.

Who owns what:

- **The application owns the run** — sequencing, routing, budgets, and the
  persisted state of a run. It decides *what happens when*, never *how* the
  outside world is reached.
- **The application owns the contract with the model** — the system-prompt
  output contract (`OUTPUT_CONTRACT` in `executor.rs`) that asks a role for a
  JSON envelope, and the parsing that tolerates markdown fences and prose
  around it.
