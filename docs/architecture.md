# Architecture

rs-nightshift is a local, overnight, multi-agent engineering harness: you leave
a goal and a config, and in the morning you review the working tree plus a
report. It never commits, pushes, resets, or cleans — that stays your call.

The harness is built hexagonally (ADR-007): a pure domain, ports, an
application layer, and adapters. This page describes that structure and the two
role-graph commands (`nightshift harness`, `nightshift plan`). For the full
`nightshift.toml` config schema and design, see
[docs/role-graph.md](role-graph.md) rather than duplicating it here; for the
module-by-module guide, see the per-folder READMEs under `src/`.

## The hexagon

```text
┌──────────────────────────────────────────────────────────────────┐
│ CLI edge — src/cli.rs + src/main.rs                              │
│ parse args, load + validate config, build adapters, exit codes   │
└──────────────────────────────┬───────────────────────────────────┘
                               │  ports (traits) — src/ports.rs
      ┌────────────────────────┼──────────────────────────┐
      ▼                        ▼                          ▼
┌───────────────┐      ┌───────────────┐          ┌──────────────────────┐
│  application  │      │    domain     │          │       adapters       │
│  orchestrator │      │  rolegraph/   │          │  providers (LLM)     │
│  executor     │      │  config       │          │  capabilities        │
│  report       │      │  verdict      │          │  artifact store      │
│  (no I/O)     │      │  routing      │          │  state store         │
│               │      │  state        │          │  clock               │
└───────────────┘      └───────────────┘          │  (the only I/O)      │
                                                  └──────────────────────┘
```

- **Domain** (`src/domain/rolegraph/`) — pure data: the `nightshift.toml`
  schema + validation, the verdict envelope, routing targets, and run-state
  models. No network, no filesystem.
- **Ports** (`src/ports.rs`) — the traits the application depends on:
  `ModelClient`, `ModelClientFactory`, `Clock`, `ArtifactStore`, `StateStore`,
  `ToolRunner`, `ContextProvider` — each shipping a test double so the
  application is unit-testable without a network, git, or a filesystem.
- **Application** (`src/application/`) — the use cases: the role executor, the
  graph orchestrator, and the terminal report. Orchestration only.
- **Adapters** (`src/adapters/`) — the implementations: LLM providers + the
  client factory, capabilities (`run-tests` / `apply-patch` /
  `write-file` / `search-replace` / `gather-context`), the filesystem artifact
  and state stores, and the system clock. The only layer that imports
  `llm-kernel` / `reqwest` or shells out.

**The rule:** domain and application never do I/O — never `std::net`,
`reqwest`, `tokio::fs`, or `std::process` directly; every side effect happens
through a port. The CLI edge (`main.rs`) builds the concrete adapters and
injects them, so the application never mentions an adapter by name.

For beta these are modules of one crate; the plan is to fragment into
`nightshift-core` / `nightshift-engine` / `nightshift-cli` once the boundaries
stabilise (ADR-007).

## The two harness commands

Both commands read the same role-graph config (`nightshift.toml` by default)
and write into a per-run directory `{date}_{slug}-run-{timestamp}` /
`{date}_{slug}-plan-{timestamp}` under the artifact root (`--out`, default
`./artifacts`). `nightshift doctor` validates the *configured* role graph
before a run: the toolchain, `codegraph` / `graphify` on PATH (only when a role
declares `gather-context`), and Ollama reachability + configured models (only
when a role uses the `ollama` provider).

### `nightshift harness` — the unattended run

```text
nightshift harness --goal "add a /health endpoint" --repo ~/projects/my-app \
                   [--config nightshift.toml] [--out ./artifacts] [--name SLUG]
```

Walks the role graph from `run.start`, routing **deterministically** on each
role's verdict (`continue` / `issues` / `questions` / `done` / `fail`) — no
model decides the next step. Loop-backs count against the role's `max_loop`;
`run.max_steps` is a global ceiling (safety backstop, not a target). On a
blocking `questions` verdict (with `on_unclear = "halt"`, the default) or an
exhausted loop budget the run halts and the report classifies why
(`ill_defined_task`, `tool_failure`, `version_mismatch`, `budget_exhausted`).

Exit codes: `0` PASSED, `1` FAILED, `2` REQUIRES_HUMAN_REVIEW.

### `nightshift plan` — the pre-flight Q&A

```text
nightshift plan --goal "add a /health endpoint" --repo ~/projects/my-app \
                [--config nightshift.toml] [--out ./artifacts] [--name SLUG]
```

Runs only the entry role and, when it raises clarifying questions, prints them
and reads your answers from stdin — repeating until no blocking questions
remain (up to 5 rounds) — then writes the brief artifact. The run directory is
tagged `-plan-` so a plan never shares a directory with a later harness run.

## Providers

Roles select a provider by name; the client factory (`build_model_client`,
`src/adapters/`) resolves it:

| Provider  | Backend           | Default                        | Auth                |
| :-------- | :---------------- | :----------------------------- | :------------------ |
| `ollama`  | built-in          | `http://127.0.0.1:11434`       | none (local)        |
| `deepseek`| OpenAI-compatible | `https://api.deepseek.com`     | `DEEPSEEK_API_KEY`  |
| `kimi`    | OpenAI-compatible | `https://api.moonshot.cn/v1`   | `MOONSHOT_API_KEY`  |
| custom    | `openai-compatible` | from `[providers.<name>]`    | `api_key_env`       |

- Ollama keeps its `keep_alive: 0` VRAM unload after each call; with
  `options.think = true` the `:think` model-tag suffix is applied
  (`phi4` → `phi4:think`).
- The model tag is passed to the provider verbatim — the config owns the exact
  tag/variant (e.g. `phi4`, `kimi3`, `deepseek-v4-pro`).
- Per-role `options` (`temperature`, `max_tokens`, `think`) are applied at the
  adapter; `num_ctx` is validated but not forwarded (llm-kernel's request has
  no such field). Unknown option keys are ignored; invalid types are config
  errors.
- Base URLs are validated (`http(s)://`, host required, no embedded
  credentials) and reported redacted.

See `src/adapters/providers/README.md` for the full provider rules.

## Capabilities

Models write text + a verdict; the harness performs the dangerous work through
the capability adapters (`src/adapters/capabilities.rs`), declared per role via
`tools = [...]`:

- **`run-tests`** — detect the test command (`cargo test`, `bundle exec rspec`,
  `mix test`, `pytest`, or `[test] command` in config), run it in the repo with
  a 600 s wall-clock timeout, and capture the exit code + output. Runs as a
  pre-tool: the results are injected into the role's prompt so it reasons over
  real results. The command never comes from model output.
- **`apply-patch`** — validate the diff paths (no `..`, no absolute paths),
  `git apply --check`, dirty-tree guard, then apply. Runs as a post-tool for
  `continue` / `done` verdicts. Never commits.
- **`write-file`** — write `content` that starts with `file: <path>` as the
  full file. Post-tool for `continue` / `done`. Path must stay in the repo.
- **`search-replace`** — exact `old:` / `new:` substitutions in existing
  files. Each `old` snippet must match once (including overlapping matches);
  not-found and ambiguous matches abort with no writes. Secret-bearing paths
  are rejected. Post-tool for `continue` / `done`. Never commits.
- **`gather-context`** — the codegraph + graphify context bundle, injected
  into the role's prompt as repo context.

## State & report

Each run persists, under its run directory:

- `actions.jsonl` — the append-only action log, one JSON object per event:
  `role_start`, `role_end`, `loop`, and the terminal `done` / `fail` / `halt`,
  carrying timestamp, role, provider/model, verdict, artifact, and block
  reason.
- `state.json` — the status snapshot: current role, step count, last verdict,
  status (`running` / `done` / `blocked` / `failed`), block reason, and
  per-edge loop counters. Written via temp-file + rename so an interruption
  never truncates it.

Role outputs (the `output = "…"` files from config, e.g. `01_brief.md`,
`02_patch.patch`) land in the same run directory. In the morning,
`nightshift harness` prints the report rendered from the snapshot and action
log (`src/application/report.rs`): status, step count, block-reason
description, and the role trail (`product-owner → developer → qa`).

## Module layout

```text
src/
├── domain/rolegraph/   # pure domain: config, verdict, routing, state (no I/O)
├── ports.rs            # port traits + test doubles
├── application/        # executor, orchestrator, report (orchestration only)
├── adapters/           # the only I/O layer: providers, capabilities, context, stores, clock
├── cli.rs              # clap parsing: doctor | status | run | harness | plan
├── main.rs             # CLI edge: wires adapters, owns process exit
└── legacy pipeline modules, being retired under ADR-006:
    pipeline.rs, pm.rs, techlead.rs, qa.rs, writer.rs, models.rs,
    artifacts/, doctor/, ollama.rs, generate.rs, testrun.rs,
    dev.rs, error.rs
```

`nightshift run` and `nightshift status` are the legacy fixed-pipeline
commands, superseded by `harness` / `plan`; the shared primitives they and the
adapters both use (`ollama.rs`, `generate.rs`, `adapters/context.rs`, `testrun.rs`,
`dev.rs`) stay until the migration completes.
