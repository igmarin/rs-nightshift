# src/domain — the pure core

The domain is the hexagonal core of the role-graph harness (ADR-007): pure
data, validation, and routing rules — no network, no filesystem, no process.
It is trivially unit-testable and depends on nothing outside the crate's own
`error.rs`. See `docs/role-graph.md` for the design and the full config schema.

`rolegraph/` holds the role-graph data model:

| Module    | Contents |
| :-------- | :------- |
| `config.rs` | the `nightshift.toml` schema (`NightshiftConfig`, `RunOptions`, `ProviderSpec`, `RoleSpec`) plus graph validation: duplicate role ids, unknown providers / routing targets / tools, `run.start` defined, `max_steps >= 1` |
| `verdict.rs` | the verdict envelope a role emits: `Verdict` (`continue` / `issues` / `questions` / `done` / `fail`), `RoleOutput`, `Question`, `BlockReason` |
| `routing.rs` | `Target` (a role id, `@done`, or `@halt`) and the per-role `Routing` map, with defaults (`continue` → `@done`, `issues` / `questions` → `@halt`) |
| `state.rs` | run state: `RunStatus`, `EventKind`, `ActionEvent` (append-only log record), `StatusSnapshot` — serde types the state adapter serializes to JSONL / JSON |

Everything is plain data with `serde` support, so the state-store adapter can
serialize the domain types without the domain knowing anything about I/O.

The one file read in the domain is the config loader
`load_role_graph_config_from` (in `config.rs`): it reads, parses, and validates
`nightshift.toml`, and is invoked by the CLI edge (`main.rs`, `doctor`) before
any application code runs. The rest of the domain never touches the filesystem.

Who owns what:

- **The domain owns the vocabulary** — what a role is, what a verdict means,
  how a verdict maps to a target, what a run's state looks like.
- **The domain owns validity** — a config that parses but is internally
  inconsistent (unknown provider, dangling route, unknown tool) is rejected
  here, before any model or tool is touched.

Ports live in `crate::ports`; adapters and orchestration live outside the
domain (see `src/adapters/README.md` and `src/application/README.md`).
