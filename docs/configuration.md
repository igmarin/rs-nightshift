# Configuration

Everything works out of the box — no config file required. This page covers the
overrides available when you need them.

## Precedence

```text
CLI flag  >  environment variable  >  nightshift.toml  >  built-in default
```

## Ollama origin

| Setting | CLI flag | Env var | Default |
| :------ | :------- | :------ | :------ |
| Ollama URL | `--ollama-url` | `NIGHTSHIFT_OLLAMA_URL` | `http://127.0.0.1:11434` |

For Ollama model tuning (CPU-only inference, Modelfile variants, `num_ctx`,
`num_thread`, `num_gpu`, GPU offload), see [`docs/ollama.md`](ollama.md).

Set via CLI flag or env var. This is **not** read from `nightshift.toml` — the
Ollama URL is a runtime connection setting, not a model mapping. Invalid URLs
(scheme other than `http`/`https`, missing host, path/query/fragment present,
userinfo credentials) are rejected by `doctor` and `run`.

## nightshift.toml

| Setting | Env var | Default |
| :------ | :------ | :------ |
| File path | `NIGHTSHIFT_CONFIG` | `nightshift.toml` (in CWD) |
| `[role_models]` table | — | built-in defaults (see below) |

A sample file is at [`nightshift.toml.example`](../nightshift.toml.example). Copy
it to `nightshift.toml` in the directory where you run `nightshift`:

```text
cp nightshift.toml.example nightshift.toml
```

## Role-to-model mapping

Override the default model for any role in `nightshift.toml`:

```toml
[role_models]
Dev = "qwen2.5-coder:14b"
Qa = "deepseek-r1:14b"
```

| Role | Default model | Responsibility |
| :--- | :--- | :--- |
| Router | `llama3.2:3b` | Fast schema repair and payload checks |
| Pm | `llama3.1:8b` | User-story writer |
| TechLead | `mistral-nemo:12b` | Architect / tech spec |
| Dev | `qwen2.5-coder:7b` | Implementation / patch author |
| Qa | `deepseek-r1:7b` | Test-failure reasoner |
| Writer | `gemma2:9b` | Changelog and article writer |
| Aux | `phi3.5:latest` | Lightweight sanity check |

Unknown role names in the config file are silently ignored — only the seven
roles above are recognized. The `doctor` command reports active overrides and
parse errors as a non-required check.

## Examples

### Point at a remote Ollama instance

```text
export NIGHTSHIFT_OLLAMA_URL=http://10.0.0.5:11434
nightshift doctor
nightshift run --goal "…" --repo ~/projects/my-app
```

### Swap in a larger Dev model

```toml
# nightshift.toml
[role_models]
Dev = "qwen2.5-coder:14b"
```

### Use a config file from a different path

```text
export NIGHTSHIFT_CONFIG=/etc/nightshift/production.toml
nightshift run --goal "…" --repo ~/projects/my-app
```

## Local OpenAI-compatible servers (rapid-mlx, llama.cpp, LM Studio)

Any server that speaks the OpenAI chat API can back a role. On an Apple
Silicon Mac, rapid-mlx serves MLX
models on `http://127.0.0.1:8000/v1`:

```text
rapid-mlx serve qwen3.6-35b-4bit      # or several models; see `rapid-mlx models`
```

```toml
[providers.mlx]
backend = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"
# no api_key_env: a keyless local server

[[roles]]
id = "developer"
provider = "mlx"
model = "qwen3.6-35b-4bit"   # an id from GET /v1/models
timeout_secs = 1800
```

Omit `api_key_env` for a server that needs no key. Nightshift then sends a
placeholder bearer token (`local`), which local servers ignore. This is only
allowed when `base_url` is this machine (`127.0.0.1`, `localhost`, `::1`); a
remote provider without `api_key_env` is a config error. Plain `http://` is
also loopback-only: use `https://` for any other host, so a key never crosses
the network in cleartext.

`nightshift doctor` calls `GET {base_url}/models` for every such provider a
role uses: `provider:<name>` fails when the server is down, and
`model:<name>:<model>` fails, listing the served models, when a role names a model the
server does not have.

## Machine profiles (`@slot` models)

To run the same roles on different machines (a Mac with rapid-mlx, a Linux
box with Ollama), give roles a **model slot** instead of a model, and map
slots to models once per machine:

```toml
[run]
start = "po"
profile = "mac"            # default; --profile / NIGHTSHIFT_PROFILE override it

[providers.mlx]
backend = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"

[profiles.mac]
planner = { provider = "mlx", model = "qwen3.8-27b-4bit", options = { max_tokens = 4096 } }
coder   = { provider = "mlx", model = "gemma-4-31b-4bit" }

[profiles.linux]
planner = { provider = "ollama", model = "llama3.1:8b" }
coder   = { provider = "ollama", model = "qwen2.5-coder:7b" }

[[roles]]
id = "po"
model = "@planner"         # no provider: the profile supplies it
options = { temperature = 0.1 }
prompt = "..."
```

- Select the profile with `--profile NAME` > `NIGHTSHIFT_PROFILE` >
  `[run] profile`. Setting `NIGHTSHIFT_PROFILE` in each machine's shell
  profile means the repo config never changes.
- A role's own `options` override the slot's options key by key.
- Roles with a literal `model` (and a `provider`) work as before. Configs
  without `@slot` models need no profile and ignore the selected one, so a
  machine-wide `NIGHTSHIFT_PROFILE` never breaks them.
- Errors at load time: an unknown profile (lists the defined ones), a slot
  the profile does not define, a slot with no profile selected, or a role
  that sets both `provider` and an `@slot` model.
- `nightshift doctor` shows the resolved models (`[ok] roles - profile mac:
  po: mlx/qwen3.8-27b-4bit, …`) and checks them. `harness` and `plan` print
  the same line before the run starts. `nightshift bench --model @coder`
  benchmarks whatever the selected profile maps `coder` to.

## Benchmark a model (`nightshift bench`)

Before a long run, check that a model works with the harness:

```bash
nightshift bench --model llama3.1:8b                       # built-in ollama
nightshift bench --provider mlx --model gemma-4-31b-4bit   # [providers.mlx] from nightshift.toml
nightshift bench --provider mlx --model qwen3.8-27b-4bit --config mac.toml --runs 5 --timeout 300
```

The client is built the same way a run builds it, so `--provider` takes
`ollama` (default), `deepseek`, `kimi`, or any `[providers.*]` name from the
config (`--config`, default `nightshift.toml` when it exists). That lets you
compare models on each machine without editing roles.

Three tasks run `--runs` times each (default 3), with a `--timeout` per call
(default 120 s):

| Task | Passes when the reply… |
| :--- | :--- |
| `json-validity` | parses as a role envelope with the same parser a run uses |
| `text-quoting` | is the two requested lines, exact (the first line may drop some of its leading spaces), with no fences or prose. Informational: a failure shows as `WARN` and does not change the verdict, because no real role replies with bare text |
| `search-replace` | is a role envelope with verdict `continue` or `done` whose `content` is one `file:`/`old:`/`new:` block that, applied to the sample file, gives the expected file (nothing is written to disk). The task sends a generic role sentence plus the contract every `search-replace` role gets; your own role prompt is not part of it |

Output shows the pass rate and the average and max latency per task. The
first call can be slow while the server loads the model. The command exits
0 only when every run of `json-validity` and `search-replace` passes:

```text
$ nightshift bench --provider mlx --model gemma-4-31b-4bit --runs 2
model: gemma-4-31b-4bit at http://127.0.0.1:8000/v1
[PASS] json-validity   2/2 passed  avg 12.5s  max 20.4s
[PASS] text-quoting    2/2 passed  avg 5.2s  max 6.2s
[PASS] search-replace  2/2 passed  avg 5.8s  max 6.9s
harness compatibility: pass
```

A `json-validity` failure usually means a thinking model spent `max_tokens`
on reasoning: raise `max_tokens` or use a non-thinking variant.

## Goal queue (`nightshift queue`)

`nightshift harness` runs one goal and exits. `nightshift queue` runs a list
of goals back to back, so the machine keeps working overnight:

```text
nightshift queue --file goals.toml --config mac.toml --out night-1
```

The queue file is an ordered list of jobs:

```toml
[[jobs]]
goal = "Add a --json flag to the status command"
repo = "/Users/me/checkouts/app-a"

[[jobs]]
goal = "Fix the flaky date parser test"
repo = "/Users/me/checkouts/app-b"
name = "date-parser"        # optional run-directory slug
config = "review-only.toml" # optional; default is the queue's --config
profile = "mac"             # optional; default is --profile / NIGHTSHIFT_PROFILE
```

Relative `repo` and `config` paths in the queue file resolve against the
queue file's own directory (not the directory you start `nightshift` in), and
are stored as absolute paths with symlinks resolved, so a link that is later
pointed at another checkout counts as a different job. The queue's `--config` flag is a normal
command-line path. Each job writes its own
dated run directory under `--out`, exactly like `harness`.

Keep `--out` outside the job repos, or add it to their `.gitignore`.
Otherwise the queue's own files make the repo dirty and its job is blocked.

| Rule | What happens |
| :--- | :--- |
| One job at a time | Jobs never run in parallel. |
| A job fails | The queue moves on to the next job. `--stop-on-failure` stops instead. |
| The job's repo has uncommitted changes | The job is skipped as `blocked: dirty_tree`. The harness never commits, so a second job on the same checkout is blocked whenever an earlier job left changes there. Jobs that edit code need their own checkout; jobs that change nothing (a review-only graph) can share one. |
| The process is killed or the machine reboots | Run the same command again. Jobs already `done` are skipped; every other job is tried again. A job that had already changed its repo is then `blocked: dirty_tree`: review that checkout, commit or restore it, and run the command once more. A `done` job is skipped only while its entry in the queue file is unchanged (same position, goal, repo, name, config and profile). |
| Two queues use the same `--out` | The second one stops with an error. A lock file (`queue.lock`) allows one queue per `--out`; the lock is released when the process ends, even after a crash. |

Two files under `--out` track the queue:

- `queue_state.json`: each job's status (`pending`, `running`, `done`,
  `blocked`, `failed`), the reason, and its run directory. Delete it to run
  every job again. Use one `--out` per queue file.
- `queue.lock`: the lock described above. It is safe to leave in place.
- `queue_report.md`: the same as a table, rewritten after every job.

The command prints the report at the end and exits 0 only when every job is
`done`; otherwise it exits 1.

## Per-role generate timeout

Each `[[roles]]` block may set `timeout_secs` for that role's LLM completion.
Omitted roles use the global default of 3600 seconds (the same value as
`DEFAULT_GENERATE_TIMEOUT` in `src/adapters/ollama.rs`). `timeout_secs = 0` is
rejected at config load.

```toml
[[roles]]
id = "product-owner"
timeout_secs = 1200  # 20 min — PO/QA often finish faster

[[roles]]
id = "developer"
timeout_secs = 3600  # 60 min — large-file generation on CPU
```

Do not put the timeout only in `options`; it is a first-class field so the
harness can bound the generate call, not a provider sampling knob.

## Per-role retries

A role gets `retries = 2` by default: up to two more attempts when its reply
is rejected. Set `retries = 0` to fail on the first slip, or raise it for a
flaky model.

```toml
[[roles]]
id = "developer"
retries = 3
```

| What failed | Retried? | Next prompt |
| :--- | :--- | :--- |
| Reply is not a valid JSON envelope | yes | same prompt plus `Your previous reply was rejected: <error>` |
| Reply was cut off at the output limit (`max_tokens`) | yes | same prompt plus the reason, so the model can send less text |
| `apply-patch`, `write-file` or `search-replace` failed | yes | same prompt plus the tool's error |
| Provider request failed (connection reset, HTTP error) | yes | same prompt |
| Timeout | no | on a slow machine the same prompt times out again |
| Model not found, config error | no | |

A retry is part of the same step, so it does not count against `max_steps`
or `max_loop`. Each one adds a `retry` event to `actions.jsonl` with the
attempt number and the reason. Each rejected reply is kept in the run
directory as `raw_response_<role>_<attempt>.txt`; a later pass through the
same role (a loop-back) overwrites them. When the last attempt fails, the
run fails as before.

A reply that the model did not finish is never used, also when its text can
be repaired into a valid envelope: it would apply half an edit, or write a
file that ends in the middle. The server reports this (`finish_reason` is
`length`). If a role keeps failing with "the reply was cut off at the output
limit", raise its `max_tokens`, or ask for less output per reply (for
example `search-replace` blocks in place of a whole file).

`retries` may be at most 10. A role with two edit tools is not retried once
the first tool has changed the repo. `plan` keeps no action log, so it
prints each retry to stderr.

## Role tools

Each `[[roles]]` block may declare `tools = [...]`. Models return text plus a
verdict; the harness performs the side effect. Unknown names are rejected at
config load.

| Tool | When it runs | What it does |
| :--- | :----------- | :----------- |
| `gather-context` | Before the LLM call | Injects codegraph/graphify context (and optional `context_files`) into the prompt |
| `run-tests` | Before the LLM call | Runs the detected test command and injects the results |
| `apply-patch` | After `continue` / `done` | Applies the role's `content` as a unified diff (`git apply --check`, then apply) |
| `write-file` | After `continue` / `done` | Writes `content` that starts with `file: <path>` as the full file. Creates the file and its directories when they do not exist. One file per reply. |
| `search-replace` | After `continue` / `done` | Replaces unique `old:` snippets with `new:` text in existing files |

### run-tests

The test command is chosen for the repo the job edits, never by a model:

| The repo has | Command |
| :--- | :--- |
| `nightshift.toml` with a `[test]` table | its `command` |
| `Cargo.toml` | `cargo test` |
| `Gemfile` | `bundle exec rspec` |
| `mix.exs` | `mix test` |
| `pyproject.toml`, `pytest.ini` or `setup.py` | `pytest` |

The first match wins. The command runs in the repo with a 600 second limit;
its exit code and output go into the role's prompt.

When the limit is reached, nightshift stops the command and every process it
started (for `cargo test`: also the test program that hangs), and the role
fails with `test command timed out`. For this, the command runs in its own
process group. One effect: Ctrl-C in the terminal stops nightshift but does
not reach a test command that is running at that moment. That command ends
at its next write to its output; one that prints nothing runs until it ends
by itself.

A test command can also exit and leave a process running, for example a
server that a test started and did not stop. nightshift does not wait for
that process and does not stop it. It takes the output written up to 2
seconds after the command's exit.

A job is "done" when the QA role says so, and the QA role sees only this
command's result. So put every check the repo's CI makes into it. The command
is split on spaces and runs without a shell (no quotes, `&&` or pipes), so
point it at a script for more than one step:

```toml
# nightshift.toml in the repo the job edits
[test]
command = "sh scripts/job-check.sh"
```

```sh
#!/bin/sh
set -e
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

This repo uses that script ([`scripts/job-check.sh`](../scripts/job-check.sh)).
A formatter that fixes the files (`cargo fmt`, not `cargo fmt --check`) saves
a round trip to the developer role.

### search-replace

Prefer this over `apply-patch` or `write-file` when the model can quote a unique
snippet but cannot emit a correct diff or regenerate a large file.

```toml
[[roles]]
id = "developer"
tools = ["gather-context", "search-replace"]
```

Put the edits in the JSON `content` field:

```text
file: public/index.html
old: <h1>Welcome</h1>
new: <h1>Hello</h1>
```

Multi-line snippets and extra `old:` / `new:` blocks (same file or another
`file:` header) are allowed. A multi-line snippet starts on the line after
`old:` or `new:`. The role's system prompt already explains this format, with
an example, so the role prompt does not need to repeat it. Each `old` text must match exactly once, including overlapping occurrences;
zero matches or two-plus matches abort the whole tool call and leave the repo
unchanged. Paths must stay inside the repo. Secret-bearing paths (`.git`,
`.env`, keys, and the rest of the `context_files` denylist) are rejected,
including symlinks that resolve into those paths. The tool never creates
files and never commits.

When an `old` text does not match, the error says where to look, and the role
gets it as feedback for its next attempt (see `retries`):

```text
search-replace: old_text not found in src/report.rs. old matches the file from line 33 for 7 line(s), then differs.
  old line 8:     if snapshot.status != RunStatus::Done {
  file line 40:     let retry_count = events
Quote the file as it is now.
```

Whitespace at the start and end of a line is ignored to find that place, and
the message says when only the whitespace differs. When `old` matches more
than once, the error lists the line of each match.

In the role's prompt, ask for short `old` blocks: the lines that change and
one or two lines around them. A long block fails on the first line that the
model does not copy exactly.
