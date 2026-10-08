# src — crate layout

`rs-nightshift` is a single crate: the library (`src/lib.rs`, the module list)
plus the `nightshift` binary (`src/main.rs`). All business logic lives in the
library; `main.rs` only parses the CLI, wires the layers together, and owns
process exit.

## Hexagonal layering (ADR-007)

The role-graph harness (`nightshift harness` / `nightshift plan`) follows
ports & adapters (hexagonal) so domain logic stays pure and every side effect
sits behind a trait. See `docs/role-graph.md` §Hexagonal architecture and
ADR-007 (`docs/decisions.md`).

| Layer       | Location                    | Responsibility                                                        | May do I/O? |
| :---------- | :-------------------------- | :-------------------------------------------------------------------- | :---------- |
| domain      | [`domain/`](domain/README.md) | role-graph config, verdicts, routing, run-state — pure data + validation | no       |
| ports       | [`ports.rs`](ports.rs)      | the traits the application depends on (`ModelClient`, `ModelClientFactory`, `Clock`, `ArtifactStore`, `StateStore`, `ToolRunner`, `ContextProvider`) plus test doubles | no |
| application | [`application/`](application/README.md) | the use cases: role executor, graph orchestrator, terminal report — orchestration only | no |
| adapters    | [`adapters/`](adapters/README.md) | implementations of the ports: LLM providers + client factory, capabilities, filesystem stores, system clock | **yes — the only layer allowed** |
| cli         | [`cli.rs`](cli.rs)          | clap argument parsing for `doctor` / `status` / `run` / `harness` / `plan` | — |

### The rule: domain and application never do I/O

Domain and application code never touch `std::net`, `reqwest`, `tokio::fs`, or
`std::process` directly — only through a port. Adapters are the only modules
that import `llm-kernel` / `reqwest` or shell out to external tools (git, test
runners, `codegraph` / `graphify`, `date`). Every port ships a test double
(`ScriptedModelClient`, `MemoryArtifactStore`, `MemoryStateStore`,
`StubToolRunner`, `StubContextProvider`, `FixedClock`), so the executor and
orchestrator are unit-testable without a network, git, or a filesystem.

The CLI edge (`main.rs`) is where concrete adapters are selected and injected:
it builds `ProviderFactory`, `FsArtifactStore`, `FsStateStore`, `SystemClock`,
`CapabilityRunner`, and `GraphContextProvider`, then hands them to the
application — the application never imports an adapter directly. The one file
read that lives inside the hexagon is the config loader
`domain::rolegraph::config::load_role_graph_config_from`, invoked by the edge
before the application runs; everything else is I/O-free by construction.

### Beyond the hexagon: legacy pipeline modules

Modules that predate the role graph and back the legacy
`nightshift run` / `nightshift status` commands are still present and are being
retired under ADR-006:

- `pipeline.rs`, `pm.rs`, `techlead.rs`, `qa.rs`, `writer.rs` — the fixed
  PM → TechLead → Dev → QA → Writer orchestration.
- `models.rs`, `artifacts/`, `doctor/` — legacy role-model config, artifact
  store + QA-report handling, and host checks (`doctor` now validates the
  *configured* role graph instead of the fixed model tags).
- Shared primitives reused by the adapters (and by the legacy pipeline):
  `ollama.rs` (`OllamaClient` + URL validation/redaction), `generate.rs`
  (`map_kernel_error`), `adapters/context.rs` (codegraph/graphify gather),
  `testrun.rs` (test-command detection + runner), `dev.rs` (`apply_checked`).
- `error.rs` — the single crate-wide `Error` enum (ADR-005).

These legacy modules do their own I/O; the hexagonal rule applies to the
role-graph layers (`domain` / `ports` / `application` / `adapters`).
