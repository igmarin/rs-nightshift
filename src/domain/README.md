# src/domain: the pure core

The domain is the core of the role-graph harness (ADR-007): plain data,
validation and routing rules, with no network, no filesystem and no
processes. Outside the domain it depends only on the crate's `error/`
module. See [role-graph.md](../../docs/role-graph.md) for the design and
[configuration.md](../../docs/configuration.md) for the config keys.

`rolegraph/` holds the role-graph data model:

| Module | Contents |
| :--- | :--- |
| `config.rs` | the `nightshift.toml` schema (`NightshiftConfig`, `RunOptions`, `ProviderSpec`, `RoleSpec`, `ModelSlot` for profiles) and its validation: duplicate role ids, unknown providers, routing targets and tools, a `run.start` that exists, `max_steps >= 1`, and the rules for profiles, `timeout_secs`, `retries` and `context_files` |
| `verdict.rs` | the envelope a role returns: `Verdict` (`continue`, `issues`, `questions`, `done`, `fail`), `RoleOutput`, `Question`, `BlockReason` |
| `routing.rs` | `Target` (a role id, `@done` or `@halt`) and the per-role `Routing` map. With no entry, `continue` goes to `@done`, and `issues` and `questions` go to `@halt` |
| `state.rs` | run state: `RunStatus`, `EventKind`, `ActionEvent` (one action-log line), `ModelCall` (an `llm_call` event's time and tokens), `StatusSnapshot`. The state adapter writes them as JSONL and JSON |

The types derive `serde`, so the config is parsed and the state adapter
writes the domain types without the domain knowing about I/O.

The domain reads one file. In `config.rs`, `read_config` reads and parses
`nightshift.toml`, and `load_role_graph_config_from` also applies the
profile and validates the graph. `main.rs` calls them before any
application code runs (`bench` uses `read_config` alone). A config that
parses but does
not hold together (an unknown provider, a route to a missing role, an
unknown tool) is rejected there, before any model or tool runs.

Ports are in `crate::ports`; the adapters and the orchestration are outside
the domain (see [adapters](../adapters/README.md) and
[application](../application/README.md)).
