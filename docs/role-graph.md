# Role-graph harness (design)

Status: **proposed** — locked from user decisions on 2026-08-19. This doc is the
source of truth for the beta rework; update it before changing behavior.

## Summary

Replace the hardcoded pipeline — the `Role` enum and the fixed
PM → TechLead → Dev → QA → Writer sequence in `run()` — with a **config-driven
role graph**.

- **Roles are data**, not code: provider + model + options + prompt + output +
  routing, declared in `nightshift.toml`.
- **The harness walks the graph** on a small deterministic *verdict* each role
  emits. No LLM guesses the next step.
- **Providers are pluggable** through the already-adopted `llm-kernel` layer.
- **Capabilities stay in code**: the harness does `git apply`, test runs, and
  context gathering on the role's behalf; models only write text + a verdict.

## Hexagonal architecture

The harness follows **ports & adapters** (hexagonal) so domain logic is pure
and all I/O sits behind traits, mirroring `brigid`'s `brigid-core` (pure domain
+ traits) / `brigid-pipeline` (orchestration + adapters) split.

Target crate layout (fragmentation is mechanical once these boundaries hold):

- **`nightshift-core`** — domain + ports, **no I/O**: the role graph (`config`,
  `verdict`, `routing`), state/report models, and the port traits. Trivially
  unit-testable; no network or filesystem.
- **`nightshift-engine`** — application + adapters: the graph orchestrator and
  role executor (application), plus adapters implementing the ports —
  llm-kernel clients (Ollama/Deepseek/Kimi), filesystem artifact store, JSONL
  action log, `git apply` / test-runner / context tools.
- **`nightshift-cli`** — thin shell: `clap` parsing, `plan`/`run`/`status`/
  `doctor`, process exit. No business logic.

For beta these land as modules (`src/domain`, `src/ports`, `src/application`,
`src/adapters`, `src/cli`) inside the current crate and are split into crates
when the boundaries stabilise.

Ports (traits the domain defines; adapters implement):

| Port | What it abstracts |
| :--- | :--- |
| `ModelClient` | one LLM call (`complete(model, prompt) -> text`) + error mapping |
| `ToolRunner` | a declared capability (`run-tests`, `apply-patch`, `write-file`, `search-replace`, `gather-context`) |
| `ArtifactStore` | create run dir, read/write artifacts, `latest` pointer |
| `StateStore` | append action-log events + write/read the status snapshot |
| `ContextProvider` | repo context (codegraph/graphify) for context injection |
| `Clock` | today's date (run-dir slug), deterministic in tests |

Invariants:

- Domain and application code never touch `std::net`, `reqwest`, `tokio::fs`,
  or `std::process` directly — only through a port.
- Adapters are the only modules that import `llm-kernel`, `reqwest`, or shell
  out to `git`/`codegraph`/`graphify`.
- Every port has a test double (`ScriptedModelClient`, `FakeToolRunner`,
  `FakeArtifactStore`, `InMemoryStateStore`, `NoneContext`, `FixedClock`).

## Goals (beta)

- Arbitrary role types (Product Owner, Developer, QA, Researcher, Writer,
  Editor, …) defined in config — no fixed role enum.
- Per-role `provider` + `model` + model-specific `options` (think/reasoning,
  temperature, …).
- Deterministic routing on a fixed verdict vocabulary.
- Providers: Ollama (local), Deepseek, Kimi/Moonshot.
- Append-only action log + resumable status snapshot + context injection
  ("small RAG": recent actions + prior artifacts, no embeddings).
- Two modes: `nightshift plan` (pre-flight human loop) and `nightshift run`
  (unattended overnight).
- Terminal report that classifies *why* the run blocked.

## Non-goals (beta)

- Parallel/branching DAGs (fan-out / fan-in / join nodes). The data model must
  not preclude them later, but beta only ships linear chain + loop-backs.
- Embedding-based RAG.
- Cline auth reuse / `@cline/sdk` orchestration (deferred post-beta).
- Mid-run crash resume (fresh attempt per run).
- crates.io publish (ADR-001 reaffirmed).

## Config schema

```toml
[run]
start       = "product-owner"   # entry role id
on_unclear  = "halt"            # halt (default) | proceed
max_steps   = 30                # global role-execution cap (safety backstop)

# Ollama needs no block (defaults to 127.0.0.1:11434); override to move it:
[providers.ollama]
base_url    = "http://127.0.0.1:11434"

[providers.deepseek]
backend     = "openai-compatible"
base_url    = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"

[providers.kimi]
backend     = "openai-compatible"
base_url    = "https://api.moonshot.cn/v1"
api_key_env = "MOONSHOT_API_KEY"

[[roles]]
id        = "product-owner"
provider  = "deepseek"
model     = "deepseek-v4-pro"
options   = { temperature = 0.2 }
prompt    = "…"                 # the job: template with {{goal}}, {{artifacts}}, {{memory}}
output    = "01_brief.md"       # artifact file it writes
on        = { continue = "developer", questions = "@halt" }

[[roles]]
id        = "developer"
provider  = "kimi"
model     = "kimi3"
options   = { think = true }
prompt    = "…"
output    = "02_patch.patch"
tools     = ["apply-patch"]     # harness applies the patch, not the model
on        = { continue = "qa", questions = "@halt" }
max_loop  = 3

[[roles]]
id        = "qa"
provider  = "ollama"
model     = "phi4"
options   = { think = true }
prompt    = "…"
output    = "03_qa_report.json"
tools     = ["run-tests"]       # harness runs the test command, not the model
on        = { issues = "developer" }   # loop back with findings
max_loop  = 3
```

- `on` maps a verdict to a target: a role id, `@done`, or `@halt`.
  `done` and `fail` are implicit terminals and need no `on` entry.
- `max_loop` caps how many times a *back-edge* (`issues`/`questions` to an
  earlier role) may fire for that role. Default 3.
- `options` is a provider/model-specific passthrough (temperature,
  think/reasoning, max_tokens, num_ctx, …) — never hardcoded.

## Verdicts

Each role returns a small structured envelope plus its artifact text:

```json
{
  "verdict": "continue",
  "summary": "one line",
  "findings": ["…"],
  "questions": [{"text": "…", "blocking": false}],
  "block_reason": null
}
```

| Verdict    | Meaning | Routing |
| :--------- | :------ | :------ |
| `continue` | work accepted, proceed | `on.continue` target (role id or `@done`) |
| `issues`   | work incomplete / bugs | `on.issues` target (loop-back), carries `findings`; counts against `max_loop` |
| `questions`| needs clarification | `on.questions` target or `@halt`; each question carries `blocking` |
| `done`     | finished successfully | terminal — run `PASSED` |
| `fail`     | hard failure | terminal — run `FAILED`/`REQUIRES_HUMAN_REVIEW`, with `block_reason` |

`block_reason` (when terminal or when a loop cap is exhausted):
`ill_defined_task`, `tool_failure`, `version_mismatch`, `budget_exhausted`,
`none`.

## Routing & loop caps

- The harness routes deterministically on `verdict`; it never asks a model
  "what next".
- Blocking `questions` in unattended mode → `@halt`: write the blocking report
  and exit `REQUIRES_HUMAN_REVIEW`. With `on_unclear = "proceed"`, non-blocking
  questions are recorded and the run continues.
- When `max_loop` is exhausted on a back-edge, the run halts with
  `block_reason = "budget_exhausted"` and a report naming the role, the edge,
  and the last findings — so the morning review says *"X of Y blocked because
  …"*.
- `max_steps` is a global safety backstop on total role executions (default 30).
  It is a ceiling, not a target: a 3-role graph with two `max_loop = 3`
  back-edges expands to ~9 executions worst case, so 5 would truncate the run
  mid-loop. 30 gives headroom while still preventing runaway.

## Capabilities (code-side tools)

The harness exposes a small, fixed set of tools a role may declare. Models
produce text + verdicts; the harness performs the dangerous work, preserving
the existing invariants (never commit/push/reset/clean; model output is never
used as argv).

- `apply-patch` — `git apply --check` first, validate patch paths stay in the
  repo, dirty-tree guard, then apply.
- `write-file` — write `content` starting with `file: <path>` as the full file.
- `search-replace` — exact `old:` / `new:` replacements in existing files.
  Unique match required; not-found and ambiguous matches are errors. Never
  creates files, never commits.
- `run-tests` — detect the test command (`cargo test`, `bundle exec rspec`,
  `mix test`, `pytest`, or from config), run it, capture exit code + tail.
- `gather-context` — the `codegraph` + `graphify` context bundle (as today).

## Providers & auth (beta)

- Ollama: local, default `http://127.0.0.1:11434`, `keep_alive: 0` unload after
  each call (preserved from today).
- Deepseek, Kimi/Moonshot: OpenAI-compatible backends via `llm-kernel`, API key
  from the env var named by `api_key_env`.
- The model string is passed through verbatim to `llm-kernel`; the operator's
  config owns the exact tag/variant (e.g. `phi4`, `qwen2.5-coder`, `gemma3`,
  `kimi3`, `deepseek-v4-pro`).

## State & action log

- `artifacts/latest/state.json` — resumable snapshot: current role, per-edge
  loop counters, total steps, last verdict, status (`running|done|blocked|failed`),
  `block_reason`.
- `artifacts/latest/actions.jsonl` — append-only, one JSON object per action:
  `{ts, event, role, provider, model, verdict, artifact, block_reason}`.
  Events: `role_start`, `llm_call`, `tool_call`, `role_end`, `loop`, `halt`,
  `done`, `fail`.
- Context injection: each role's prompt gets the goal, the prior artifacts (or
  their summaries), and the last N action-log entries as run memory.

## Interaction modes

- `nightshift plan --goal "…" [--repo PATH]` — pre-flight: run the entry role,
  print its clarifying questions, accept answers, repeat until no blocking
  questions remain, write the brief. Fast, interactive, not overnight.
- `nightshift run --goal "…" --repo PATH` — unattended: walk the graph, loop
  within caps, hard-stop + report on a blocking issue, exit
  `PASSED` / `FAILED` / `REQUIRES_HUMAN_REVIEW`. Review in the morning.

## Migration

- Retire the `Role` enum (`models.rs`) and the hardcoded `run()` orchestration
  (`pipeline.rs`), and the per-stage orchestration in
  `pm/techlead/dev/qa/writer.rs`.
- Keep reusable primitives as capabilities: test-command detection, git apply +
  path validation, `codegraph`/`graphify` context probe, `map_kernel_error`,
  Ollama unload.
- `nightshift doctor` validates the *configured* providers/models/tools instead
  of the fixed seven Ollama tags.
- Ship example configs: `nightshift.toml.example` (the PO→Dev→QA example) plus a
  non-engineering example (e.g. Researcher → Writer → Editor) to prove roles are
  domain-neutral.
