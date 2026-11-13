# Role-graph harness (design)

This is the design of the role graph as it shipped in the beta
(`nightshift harness`, `plan` and `queue`). It explains why the graph works
the way it does. For every config key, see
[configuration.md](configuration.md).

## Summary

The role graph replaces the fixed pipeline of `nightshift run` (PM → Tech
Lead → Dev → QA → Writer, with a hardcoded `Role` enum) with roles declared
in config.

- Roles are data: provider, model, options, prompt, output file, tools and
  routing, declared in `nightshift.toml`.
- The harness walks the graph on a small, fixed set of verdicts that each
  role returns. No model decides the next step.
- Providers are pluggable through the `llm-kernel` crate.
- Side effects stay in code. The harness applies edits, runs tests and
  gathers context for a role; models only return text and a verdict.

The fixed pipeline still exists behind `nightshift run` and
`nightshift status`. It is retired once the role graph covers it (ADR-006,
[Migration](#migration)).

## Hexagonal architecture

The harness uses ports and adapters (ADR-007), modeled on the `brigid` repo:
`brigid-core` holds the domain and traits, `brigid-pipeline` the
orchestration and adapters. Domain logic stays pure, and I/O sits behind
traits.

Today it is one crate with the modules `src/domain`, `src/ports.rs`,
`src/application`, `src/adapters` and the CLI edge (`src/cli.rs`,
`src/main.rs`). The planned split into crates has not happened yet:

- `nightshift-core`: the domain and the ports, no I/O.
- `nightshift-engine`: the application and the adapters.
- `nightshift-cli`: argument parsing, the commands, process exit.

The ports are traits in `src/ports.rs`; the adapters implement them.

| Port | What it abstracts | Test double |
| :--- | :--- | :--- |
| `ModelClient` | one model call: `generate`, `generate_with_usage` (adds the token counts), `redacted_origin` | `ScriptedModelClient` |
| `ModelClientFactory` | builds a `ModelClient` from a role's provider, model and options | none; tests define their own |
| `ToolRunner` | a declared tool: `run-tests`, `apply-patch`, `write-file`, `search-replace` | `StubToolRunner` |
| `ContextProvider` | repo context for `gather-context` (codegraph, graphify) | `StubContextProvider` |
| `ArtifactStore` | create a run directory, read and write artifacts | `MemoryArtifactStore` |
| `StateStore` | append action-log events, write and read the status snapshot | `MemoryStateStore` |
| `Clock` | the current time, and today's date for the run-directory name | `FixedClock` |

Rules:

- Domain and application code do not use `std::net`, `reqwest`, `tokio::fs`
  or `std::process`. They go through a port. Two exceptions read files: the
  config loader in the domain (`load_role_graph_config_from`, which the CLI
  edge calls before a run) and the executor, which reads a role's
  `context_files` from the repo with `std::fs`.
- In the role-graph layers, only adapters import `llm-kernel` or `reqwest`,
  or run `git`, `codegraph` and `graphify`. The legacy modules
  (`generate.rs`, `doctor/`) still import them directly.

## Goals (beta)

- Any role type (Product Owner, Developer, QA, Researcher, Writer, Editor)
  defined in config, with no fixed role enum.
- Per-role `provider`, `model` and model-specific `options` (thinking,
  temperature, output limit).
- Routing on a fixed verdict vocabulary.
- Providers: Ollama, Deepseek, Kimi/Moonshot, and any OpenAI-compatible
  server (rapid-mlx, llama.cpp, LM Studio).
- An append-only action log, a status snapshot, and context injection: the
  goal, tool output and earlier artifacts go into each prompt. No
  embeddings.
- Two modes: `nightshift plan` (questions before the run) and
  `nightshift harness` (unattended). `nightshift queue` runs many harness
  jobs back to back.
- A report that says why a run stopped.

## Non-goals (beta)

- Parallel or branching graphs (fan-out, fan-in, joins). The data model must
  not rule them out, but the beta only ships a linear chain with
  loop-backs.
- Embedding-based retrieval.
- Reusing Cline's auth or `@cline/sdk` orchestration (after the beta).
- Resuming a run after a crash. Each run starts fresh; `nightshift queue`
  restarts unfinished jobs from the beginning.
- Publishing to crates.io (ADR-001).

## Config shape

```toml
[run]
start       = "product-owner"   # entry role id
on_unclear  = "halt"            # halt (default) | proceed
max_steps   = 30                # cap on role executions in one run

[providers.deepseek]
backend     = "openai-compatible"
base_url    = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"

[[roles]]
id        = "product-owner"
provider  = "deepseek"
model     = "deepseek-v4-pro"
options   = { temperature = 0.2 }
prompt    = "…"                 # used as the system prompt, as written
output    = "01_brief.md"       # artifact file it writes
on        = { continue = "developer", questions = "@halt" }

[[roles]]
id        = "developer"
provider  = "ollama"
model     = "qwen2.5-coder:7b"
prompt    = "…"
output    = "02_change.txt"
tools     = ["gather-context", "search-replace"]
on        = { continue = "qa" }

[[roles]]
id        = "qa"
provider  = "ollama"
model     = "llama3.1:8b"
prompt    = "…"
output    = "03_qa.md"
tools     = ["run-tests"]       # the harness runs the test command, not the model
on        = { issues = "developer" }   # loop back with findings
max_loop  = 3
```

- `on` maps a verdict to a target: a role id, `@done` or `@halt`. When a
  verdict has no entry, `continue` goes to `@done`, and `issues` and
  `questions` go to `@halt`. `done` and `fail` always end the run.
- `max_loop` caps how many times a role may loop back to the same target.
  Each edge (`qa:developer`) has its own counter. Default 3.
- `retries` is how many more attempts a role gets when its reply is
  rejected or the provider request fails. Default 2; see
  [Per-role retries](configuration.md#per-role-retries).
- `options` passes provider and model settings through, so none are
  hardcoded. `temperature`, `max_tokens` and `think` (Ollama only) are
  applied; `num_ctx` is accepted but not sent. Other keys are ignored.
- Profiles let a role name a model slot (`model = "@coder"`) that each
  machine maps to its own provider and model.

## Verdicts

Each role returns one JSON object:

```json
{
  "verdict": "continue",
  "summary": "one line",
  "content": "the artifact text, written to the role's output file",
  "findings": ["…"],
  "questions": [{"text": "…", "blocking": false}],
  "block_reason": "none"
}
```

| Verdict | Meaning | Routing |
| :--- | :--- | :--- |
| `continue` | work accepted | `on.continue` (a role id or `@done`) |
| `issues` | work incomplete or buggy | `on.issues`, a loop-back that carries `findings` and counts against `max_loop` |
| `questions` | needs clarification | see [Routing and loop caps](#routing-and-loop-caps) |
| `done` | finished | ends the run: `PASSED` |
| `fail` | hard failure | ends the run: `FAILED`, with the role's `block_reason` (`tool_failure` when it gives none) |

`block_reason` is one of `ill_defined_task`, `tool_failure`,
`version_mismatch`, `budget_exhausted` or `none`.

## Routing and loop caps

- The harness routes on `verdict` alone and never asks a model what to do
  next.
- `questions`: when no question is blocking, the run follows `on.continue`.
  When one is blocking, it follows `on.questions`. By default that is
  `@halt`: the run stops as `REQUIRES_HUMAN_REVIEW` with
  `ill_defined_task`. It can also name a role, which then gets the
  questions. With `on_unclear = "proceed"`, blocking questions follow
  `on.continue` too. The questions are not saved anywhere else.
- When a loop-back would pass `max_loop`, the run stops as
  `REQUIRES_HUMAN_REVIEW` with `budget_exhausted`. The `halt` event in the
  action log names the role.
- `max_steps` caps the role executions in one run (default 30), and stops
  the run the same way. It is a ceiling, not a target: a 3-role graph with
  two `max_loop = 3` back-edges needs about 9 executions at worst, so 5
  would cut a run off in the middle of a loop. 30 leaves room and still
  stops a runaway.

## Capabilities (tools in code)

A role can declare these tools. Models return text and a verdict; the
harness does the risky work. It never commits, pushes, resets or cleans,
and never uses model output as a command or its arguments.

- `apply-patch`: checks that the diff's paths stay in the repo, then runs
  `git apply --check` and applies. When the check fails, it repairs the
  line counts in the hunk headers and checks again. When that fails too, it
  tries `git apply --3way`. It does not check for uncommitted changes;
  `nightshift queue` skips a job whose repo has them.
- `write-file`: writes `content` that starts with `file: <path>` as the
  whole file. Creates the file and its directories when they do not exist.
- `search-replace`: exact `old:` / `new:` replacements in existing files.
  Each `old` must match exactly once; no match, or more than one, is an
  error and nothing is written. Never creates files.
- `run-tests`: picks the test command (`[test] command` in the repo's
  `nightshift.toml`, else `cargo test`, `bundle exec rspec`, `mix test` or
  `pytest`), runs it with a 600 s limit, and gives the role the exit code
  and up to 1 MiB of each output stream.
- `gather-context`: the codegraph and graphify context bundle, plus the
  role's `context_files`.

## Providers and auth

- Ollama: local, default `http://127.0.0.1:11434` (`[providers.ollama]
  base_url` moves it). After each call, nightshift sends `keep_alive: 0`,
  which unloads the model; `keep_loaded = true` turns that off.
- Deepseek and Kimi/Moonshot: built-in OpenAI-compatible providers. The API
  key comes from `DEEPSEEK_API_KEY` or `MOONSHOT_API_KEY`, or from the
  variable that `api_key_env` names.
- Any other OpenAI-compatible server: a `[providers.<name>]` block with
  `backend = "openai-compatible"` and `base_url`. `api_key_env` may be left
  out only when `base_url` is this machine (a local server such as
  rapid-mlx). Plain `http://` is also allowed only for this machine.
- The model string goes to the provider as written. The config owns the
  exact tag (`qwen2.5-coder:7b`, `qwen3.6-35b-4bit`, `deepseek-v4-pro`).

## State and action log

Each run writes into `<out>/{date}_{slug}-run-{timestamp}/`:

- `state.json`: the status snapshot: current role, loop counters per edge,
  steps, last verdict, status (`running`, `done`, `blocked`, `failed`) and
  `block_reason`. It is not used to resume a run.
- `actions.jsonl`: append-only, one JSON object per event:
  `{ts, event, role, provider, model, verdict, artifact, block_reason, detail, call}`.
  The events are `role_start`, `llm_call`, `retry`, `role_end`, `loop`,
  `halt`, `done` and `fail`. An `llm_call` event's `call` holds
  `duration_ms` and, when the server reports them, `prompt_tokens` and
  `completion_tokens`. (`tool_call` exists in the schema but is not
  written.)

Each role's prompt is built from:

- the system prompt: the role's `prompt` as written, then the output
  contract (plus the search-replace format when the role has that tool);
- the user prompt: the goal, the output of the role's tools, the findings or
  questions from a loop-back, the answers given in `plan`, and the full
  text of the earlier roles' artifacts.

## Interaction modes

- `nightshift plan --goal "…" --repo PATH`: runs only the entry role, with
  its tools. While the role returns `questions`, it prints them and reads
  your answers from stdin, for up to 5 rounds or until stdin ends. Then it
  writes the brief.
- `nightshift harness --goal "…" --repo PATH`: unattended. Walks the graph,
  loops within the caps, and stops with a report. Exit code 0 is `PASSED`,
  1 `FAILED`, 2 `REQUIRES_HUMAN_REVIEW`.
- `nightshift queue --file goals.toml`: runs many harness jobs back to back
  ([Goal queue](configuration.md#goal-queue-nightshift-queue)).

## Migration

Done:

- The reusable parts of the old pipeline became adapters: test-command
  detection (`adapters/test.rs`), `git apply` with path checks
  (`adapters/git.rs`), the codegraph and graphify probe
  (`adapters/context.rs`), `map_kernel_error` (`adapters/kernel_error.rs`)
  and the Ollama unload (`adapters/ollama.rs`).
- `nightshift doctor` checks the configured providers, models and tools
  instead of seven fixed Ollama tags.
- `nightshift.toml.example` ships the PO → Dev → QA graph and, commented
  out, a Researcher → Writer → Editor graph that shows roles are not tied to
  engineering.

Not done yet: removing the `Role` enum (`models.rs`), the fixed pipeline
(`pipeline.rs`, `pm.rs`, `techlead.rs`, `dev.rs`, `qa.rs`, `writer.rs`) and
the `run` and `status` commands that use them.
