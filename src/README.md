# src: crate layout

`rs-nightshift` is one crate: the library (`src/lib.rs`, the module list)
and the `nightshift` binary (`src/main.rs`). The logic lives in the library.
`main.rs` parses the command line, builds the adapters and owns process exit.

## Layers (ADR-007)

The role-graph harness (`nightshift harness`, `plan` and `queue`) uses ports
and adapters, so the domain stays pure and side effects sit behind traits.
See [Hexagonal architecture](../docs/role-graph.md#hexagonal-architecture)
and ADR-007 in [decisions.md](../docs/decisions.md).

| Layer | Location | What it holds | Does I/O? |
| :--- | :--- | :--- | :--- |
| domain | [`domain/`](domain/README.md) | role-graph config, verdicts, routing, run state, and their validation | no, except the config loader |
| ports | [`ports.rs`](ports.rs) | the traits the application depends on (`ModelClient`, `ModelClientFactory`, `Clock`, `ArtifactStore`, `StateStore`, `ToolRunner`, `ContextProvider`) and test doubles | no |
| application | [`application/`](application/README.md) | the role executor, the graph orchestrator, the terminal report | no, except reading `context_files` |
| adapters | [`adapters/`](adapters/README.md) | the port implementations: providers and client factory, tools, context, git, test runner, file stores, clock | yes |
| cli | [`cli.rs`](cli.rs) | clap parsing for `doctor`, `status`, `run`, `harness`, `plan`, `queue` and `bench` | no |

### Domain and application do no I/O

Domain and application code do not use `std::net`, `reqwest`, `tokio::fs` or
`std::process`; they go through a port. Two exceptions read files:

- the config loader `domain::rolegraph::config::load_role_graph_config_from`,
  which `main.rs` calls before the application runs;
- `application::executor`, which reads a role's `context_files` from the
  repo.

Among the role-graph layers, only adapters import `llm-kernel` or `reqwest`
or start other programs (git, test runners, `codegraph`, `graphify`,
`date`). Every port except `ModelClientFactory` has a test double in
`ports.rs` (`ScriptedModelClient`, `MemoryArtifactStore`, `MemoryStateStore`,
`StubToolRunner`, `StubContextProvider`, `FixedClock`), so the executor and
the orchestrator are tested without a network, git or a filesystem.

`main.rs` picks the adapters and passes them in: `ProviderFactory`,
`FsArtifactStore`, `FsStateStore`, `SystemClock`, `CapabilityRunner` and
`GraphContextProvider`. The application never imports an adapter.

## Other modules

- `queue.rs`: `nightshift queue`, which runs many harness jobs in a row.
- `bench.rs`: `nightshift bench`, which checks a model against the harness.
- `doctor/`: the checks of `nightshift doctor`. It does its own I/O
  (looking up programs, HTTP requests to model servers).
- `error/`: the crate-wide `Error` (ADR-005) and its parts:
  `ProviderError`, `GitError`, `ConfigError` and `ArtifactError`.

The legacy pipeline behind `nightshift run` and `nightshift status` is still
here, to be retired (ADR-006). It does its own I/O:

- `pipeline.rs`, `pm.rs`, `techlead.rs`, `dev.rs`, `qa.rs`, `writer.rs`: the
  fixed PM → Tech Lead → Dev → QA → Writer stages.
- `models.rs`: the `Role` enum and the `[role_models]` table.
- `artifacts/`: the legacy run directory, its `latest` pointer and the QA
  report.
- `generate.rs`: text generation over llm-kernel's `LLMClient`. The Ollama
  provider adapter also uses its `Origin`.
