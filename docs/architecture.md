# Architecture

rs-nightshift runs a goal through a graph of model roles while nobody
watches. In the morning you review the working tree and a report. It never
commits, pushes, resets or cleans; that stays your call.

The harness is built as ports and adapters (ADR-007): a pure domain, ports,
an application layer and adapters. This page describes that structure and
the role-graph commands. The design and its reasons are in
[role-graph.md](role-graph.md), every config key is in
[configuration.md](configuration.md), and each folder under `src/` has a
README for its modules.

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

- Domain (`src/domain/rolegraph/`): plain data. The `nightshift.toml` schema
  and its validation, the verdict envelope, routing targets and run-state
  types.
- Ports (`src/ports.rs`): the traits the application depends on
  (`ModelClient`, `ModelClientFactory`, `Clock`, `ArtifactStore`,
  `StateStore`, `ToolRunner`, `ContextProvider`) and test doubles for them,
  so the application is tested without a network, git or a filesystem.
- Application (`src/application/`): the role executor, the graph
  orchestrator and the terminal report.
- Adapters (`src/adapters/`): the model providers and the client factory,
  the tools (`run-tests`, `apply-patch`, `write-file`, `search-replace`,
  `gather-context`), the filesystem artifact and state stores, and the
  system clock. In the role-graph layers, only adapters import `llm-kernel`
  or `reqwest` or start other programs.

The rule: domain and application code do not use `std::net`, `reqwest`,
`tokio::fs` or `std::process`; side effects go through a port. Two
exceptions read files: the config loader in the domain, which the CLI edge
calls before a run, and the executor, which reads a role's `context_files`
from the repo. The CLI edge (`main.rs`) builds the adapters and passes them
in, so the application never names an adapter.

These are modules of one crate. The plan is to split them into
`nightshift-core`, `nightshift-engine` and `nightshift-cli` crates once the
boundaries settle (ADR-007).

## The harness commands

`harness`, `plan` and `queue` read the same role-graph config
(`nightshift.toml` by default, or `--config`). Each run writes into its own
directory under `--out` (default `./artifacts`):
`{date}_{slug}-run-{timestamp}` for `harness` and `queue` jobs,
`{date}_{slug}-plan-{timestamp}` for `plan`.

`nightshift doctor` checks the configured role graph before a run: the
config itself, the Rust toolchain, `codegraph` and `graphify` (only when a
role declares `gather-context`), Ollama and its models (only for roles on
the `ollama` provider), each OpenAI-compatible server and the models the
roles name, and, on macOS, `caffeinate`. It warns when a role sets
`num_ctx`, which is not sent.

### `nightshift harness`: the unattended run

```text
nightshift harness --goal "add a /health endpoint" --repo ~/projects/my-app \
                   [--config nightshift.toml] [--out ./artifacts] [--name SLUG]
```

Walks the role graph from `run.start` and routes on each role's verdict
(`continue`, `issues`, `questions`, `done`, `fail`); no model decides the
next step. Loop-backs count against the role's `max_loop`, and
`run.max_steps` caps the whole run. On a blocking question (with the
default `on_unclear = "halt"`) or a used-up loop budget, the run stops and
the report says why (`ill_defined_task`, `tool_failure`,
`version_mismatch`, `budget_exhausted`).

Exit codes: `0` PASSED, `1` FAILED, `2` REQUIRES_HUMAN_REVIEW.

### `nightshift queue`: many runs in a row

Runs the jobs of a queue file one after another, each like `harness`, and
keeps `queue_state.json` and `queue_report.md` under `--out`. See
[Goal queue](configuration.md#goal-queue-nightshift-queue).

### `nightshift plan`: questions before the run

```text
nightshift plan --goal "add a /health endpoint" --repo ~/projects/my-app \
                [--config nightshift.toml] [--out ./artifacts] [--name SLUG]
```

Runs only the entry role, with its tools. While the role returns questions,
it prints them and reads your answers from stdin, for up to 5 rounds or
until stdin ends. Then it writes the brief. The run directory is tagged
`-plan-`, so a plan never shares a directory with a later harness run.

## Providers

Roles name a provider; the client factory (`build_model_client` in
`src/adapters/providers/factory.rs`) builds the client:

| Provider  | Backend           | Default                        | Auth                |
| :-------- | :---------------- | :----------------------------- | :------------------ |
| `ollama`  | built-in          | `http://127.0.0.1:11434`       | none (local)        |
| `deepseek`| OpenAI-compatible | `https://api.deepseek.com`     | `DEEPSEEK_API_KEY`  |
| `kimi`    | OpenAI-compatible | `https://api.moonshot.cn/v1`   | `MOONSHOT_API_KEY`  |
| custom    | `openai-compatible` | from `[providers.<name>]`    | `api_key_env`, or none for a keyless local server (rapid-mlx) |

- Ollama: after each call, nightshift sends `keep_alive: 0`, which unloads
  the model. With `options.think = true` it adds the `:think` tag suffix
  (`phi4` becomes `phi4:think`).
- The model tag goes to the provider as written; the config owns the exact
  tag (`qwen2.5-coder:7b`, `qwen3.6-35b-4bit`, `deepseek-v4-pro`).
- Per-role `options` (`temperature`, `max_tokens`, `think`) are applied in
  the adapter. `num_ctx` is checked but not sent, because llm-kernel's
  request has no such field. Unknown keys are ignored; a wrong type is a
  config error.
- Base URLs must be `http://` or `https://` with a host, and have no
  credentials, query or fragment. Plain `http://` and a missing
  `api_key_env` are allowed only for this machine (loopback). Errors show
  the URL with credentials removed.

See [the providers README](../src/adapters/providers/README.md) for the
details.

## Capabilities

Models return text and a verdict; the harness does the risky work through
the tools a role declares with `tools = [...]`
(`src/adapters/capabilities.rs`):

- `run-tests`: picks the test command (`[test] command` in the repo's
  `nightshift.toml`, else `cargo test`, `bundle exec rspec`, `mix test` or
  `pytest`), runs it in the repo with a 600 s limit, and keeps the exit code
  and up to 1 MiB of each output stream. The command runs in its own
  process group, and the time limit stops the whole group. It runs before
  the model call, so the role sees real results. The command never comes
  from model output.
- `apply-patch`: checks that the diff's paths stay in the repo (no `..`, no
  absolute paths), runs `git apply --check`, then applies. When the check
  fails, it repairs the line counts in the hunk headers and checks again;
  when that fails too, it tries `git apply --3way`. Runs after a `continue`
  or `done` verdict. Never commits.
- `write-file`: writes `content` that starts with `file: <path>` as the
  whole file, creating it and its directories when they do not exist. Runs
  after `continue` or `done`. The path must stay in the repo and must not
  be a secret-bearing path, also through a symlinked directory.
- `search-replace`: exact `old:` / `new:` replacements in existing files.
  Each `old` must match exactly once, overlapping matches included; no
  match, or more than one, is an error and nothing is written.
  Secret-bearing paths are rejected. Runs after `continue` or `done`. Never
  commits.
- `gather-context`: the codegraph and graphify context bundle, plus the
  role's `context_files`, added to the prompt.

## State & report

Each run persists, under its run directory:

- `actions.jsonl` — the append-only action log, one JSON object per event:
  `role_start`, `retry`, `role_end`, `loop`, and the terminal `done` /
  `fail` / `halt`, carrying timestamp, role, provider/model, verdict,
  artifact, and block reason. A `retry` event also carries `detail` (the
  attempt number and why the reply was rejected).
- `state.json` — the status snapshot: current role, step count, last verdict,
  status (`running` / `done` / `blocked` / `failed`), block reason, and
  per-edge loop counters. Written via temp-file + rename so an interruption
  never truncates it.

Role outputs (the `output = "…"` files from config, such as `01_brief.md`)
land in the same run directory. At the end, `nightshift harness` prints the
report built from the snapshot and the action log
(`src/application/report.rs`): status, steps, retries, block reason, the
role trail (`product-owner → developer → qa`) and the time per role.

## Module layout

```text
src/
├── domain/rolegraph/   # config, verdict, routing, state (no I/O)
├── ports.rs            # port traits and test doubles
├── application/        # executor, orchestrator, report
├── adapters/           # providers, tools, context, git, test runner, stores, clock
├── error/              # the crate-wide Error and its parts (provider, git, config, artifact)
├── queue.rs            # nightshift queue
├── bench.rs            # nightshift bench
├── doctor/             # nightshift doctor checks (does its own I/O)
├── cli.rs              # clap parsing: doctor, status, run, harness, plan, queue, bench
├── main.rs             # CLI edge: builds the adapters, owns process exit
└── legacy pipeline, to be retired (ADR-006):
    pipeline.rs, pm.rs, techlead.rs, dev.rs, qa.rs, writer.rs,
    models.rs, artifacts/, generate.rs
```

`nightshift run` and `nightshift status` are the legacy fixed-pipeline
commands; `harness`, `plan` and `queue` replace them. The parts both use
already live in `adapters/` (`ollama.rs`, `kernel_error.rs`, `context.rs`,
`test.rs`, `git.rs`). `generate.rs` (the `LLMClient` re-export and
`Origin`) is still shared with the provider adapters.
