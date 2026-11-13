# Configuration

`harness`, `plan`, `queue`, `doctor` and `bench` read one TOML file:
`nightshift.toml` in the current directory, or the file given with
`--config`. Start from [`nightshift.toml.example`](../nightshift.toml.example).
The legacy `nightshift run` has its own settings: see
[legacy-run.md](legacy-run.md).

A config has four parts:

```toml
[run]                  # where the graph starts, and its limits
[providers.<name>]     # model servers beyond the built-in Ollama
[profiles.<name>]      # per-machine models for @slot roles (optional)
[[roles]]              # one block per role, in any order
```

## `[run]`

| Key | Default | What it does |
| :--- | :--- | :--- |
| `start` | (required) | the id of the first role |
| `max_steps` | `30` | most role turns in one run; reaching it stops the run as `REQUIRES_HUMAN_REVIEW` (`budget_exhausted`) |
| `on_unclear` | `"halt"` | what blocking questions do; see [Routing](#routing-on) |
| `profile` | none | the `[profiles.*]` to use; see [Machine profiles](#machine-profiles-slot-models) |

## `[[roles]]`

| Key | Default | What it does |
| :--- | :--- | :--- |
| `id` | (required) | the role's name, used by `start` and in routes |
| `model` | (required) | the model id the provider serves (passed as is), or `@slot` to take provider, model and options from the profile |
| `provider` | (required without `@slot`) | `ollama`, or the name of a `[providers.*]` block |
| `prompt` | `""` | the role's instructions, sent as the system prompt |
| `output` | none | the file in the run directory that gets the reply's `content` |
| `tools` | `[]` | what nightshift does for the role; see [Role tools](#role-tools) |
| `context_files` | `[]` | repo files to put in the prompt; see [`context_files`](#context_files) |
| `on` | see below | the next role for each verdict |
| `max_loop` | `3` | how often an `issues` or `questions` route from this role may send the run back |
| `retries` | `2` | extra attempts when a reply is rejected; see [Per-role retries](#per-role-retries) |
| `timeout_secs` | `3600` | the limit for one model call; see [Per-role generate timeout](#per-role-generate-timeout) |
| `options` | `{}` | model options; see [Options](#options) |

Nightshift adds a fixed note to the system prompt that asks for a JSON reply
(the verdict, a summary, findings, questions and `content`) and, for edit
tools, explains their format. The user prompt holds the goal, the tool output
(context, file contents, test results), the findings or questions from the
role that sent the run back, and the full text of the earlier roles' output
files.

### Routing: `on`

```toml
on = { continue = "qa", issues = "developer", questions = "@halt" }
```

A target is a role id, `@done` (the run passed) or `@halt` (the run stops as
`REQUIRES_HUMAN_REVIEW`). Missing entries default to `continue = "@done"`,
`issues = "@halt"` and `questions = "@halt"`.

| Verdict | Where the run goes |
| :--- | :--- |
| `continue` | `on.continue` |
| `issues` | `on.issues`, with the role's findings; counts against `max_loop` |
| `questions` | when no question is blocking, or `[run] on_unclear = "proceed"`: `on.continue`. Otherwise `on.questions`, with the questions |
| `done` | the run ends as `PASSED` |
| `fail` | the run ends as `FAILED` |

### Options

| Option | What it does |
| :--- | :--- |
| `temperature` | `0.0` to `2.0`; default `0.2` |
| `max_tokens` | the longest reply; a reply cut off at this limit is rejected and retried |
| `think` | Ollama only: `true` asks for the model's `:think` tag (`phi4` becomes `phi4:think`) |
| `num_ctx` | accepted but not sent to the server; set the context size in the model itself (Ollama: `PARAMETER num_ctx` in a Modelfile). `doctor` warns when a role sets it |

Other keys are ignored. A value of the wrong type is a config error.

### `context_files`

```toml
tools = ["gather-context", "search-replace"]
context_files = ["src/report.rs", "public/index.html"]
```

`gather-context` reads these files and adds them to the prompt. Rules:

- at most 10 files, each cut at 64 KiB;
- paths relative to the repo, with no `..`;
- the role must declare `gather-context`, which also needs `codegraph` on
  `PATH` (it runs `codegraph init` in the repo when there is no index);
- secret-bearing paths are refused: `.git/`, `.env` and `.env.*`, `.ssh/`,
  `.aws/`, `.azure/`, `.kube/`, `.npmrc`, `.netrc`, `.pypirc`, SSH keys
  (`id_rsa`, `id_ed25519`, `id_ecdsa`, `id_dsa`), `*.pem`, `*.key`, `*.p12`,
  `*.pfx`, `*.keystore`, `credentials.json` and `service-account.json`.

## Providers

### Ollama

The provider `ollama` is built in at `http://127.0.0.1:11434`. To use another
address:

```toml
[providers.ollama]
base_url = "http://10.0.0.5:11434"
```

After every call, nightshift asks Ollama to unload the model (`keep_alive:
0`), so the next call loads it again. When the models a config uses fit in
memory together, turn that off:

```toml
[providers.ollama]
keep_loaded = true
```

Ollama then keeps each model loaded for its own keep-alive time
(`OLLAMA_KEEP_ALIVE`, 5 minutes by default) and unloads an idle model
itself when it needs the memory for another one. `keep_loaded` on any other
provider is a config error.

`doctor` checks Ollama at `--ollama-url` (or `NIGHTSHIFT_OLLAMA_URL`,
default `http://127.0.0.1:11434`), not at this `base_url`, so pass both when
you move Ollama. For CPU-only machines, see [`ollama.md`](ollama.md).

### Local OpenAI-compatible servers (rapid-mlx, llama.cpp, LM Studio)

Any server that speaks the OpenAI chat API can back a role. On an Apple
Silicon Mac, rapid-mlx serves MLX models on `http://127.0.0.1:8000/v1`:

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
the network in cleartext. A `base_url` with a query string or fragment is
rejected.

`nightshift doctor` calls `GET {base_url}/models` for every such provider a
role uses: `provider:<name>` fails when the server is down, and
`model:<name>:<model>` fails, listing the served models, when a role names a
model the server does not have.

### Hosted APIs

```toml
[providers.deepseek]
backend = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
```

The key is read from the environment variable that `api_key_env` names. For
the provider names `deepseek` and `kimi`, `base_url` and `api_key_env`
default to DeepSeek's and Moonshot's (`DEEPSEEK_API_KEY`,
`MOONSHOT_API_KEY`), but the `[providers.*]` block must still exist.

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

## Per-role generate timeout

`timeout_secs` limits one model call of the role. Omitted, it is 3600 seconds
(`DEFAULT_GENERATE_TIMEOUT_SECS` in `src/domain/rolegraph/config.rs`).
`timeout_secs = 0` is rejected at config load.

```toml
[[roles]]
id = "product-owner"
timeout_secs = 1200  # a brief is short

[[roles]]
id = "developer"
timeout_secs = 3600  # whole files on a CPU take long
```

It is a role key, not an `options` entry, because nightshift enforces it;
the provider never sees it.

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
run fails.

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
verdict; nightshift does the work. Unknown names are rejected at config
load.

| Tool | When it runs | What it does |
| :--- | :----------- | :----------- |
| `gather-context` | Before the model call | Adds codegraph and graphify context, and the `context_files`, to the prompt |
| `run-tests` | Before the model call | Runs the repo's test command and adds the result to the prompt |
| `apply-patch` | After `continue` / `done` | Applies the reply's `content` as a unified diff |
| `write-file` | After `continue` / `done` | Writes `content` that starts with `file: <path>` as the whole file |
| `search-replace` | After `continue` / `done` | Replaces unique `old:` snippets with `new:` text in existing files |

### apply-patch

Every path the diff touches must stay in the repo (no `..`, no absolute
paths), must not be a secret-bearing path (the
[`context_files`](#context_files) list), and must not go through a symlink
in the working tree (nor be one). Nightshift gets the paths from git
itself (`git apply --numstat`), so new, deleted, binary, renamed and copied
files, and names git writes in quotes, are all checked. A patch that breaks
this, or that git cannot read, is rejected before anything is applied. So
is a rename or copy whose old name is in git's quoted form (`rename from
"caf\303\251.txt"`) or has a control character; write the name as it is
(`rename from café.txt`). The diff is
checked with `git apply --check`. When that fails, nightshift fixes
the line counts in the hunk headers and checks again. When that also fails,
it tries `git apply --3way`, which stages the result in the index and may
leave conflict markers in a file; check `git status` after such a run.

### write-file

The first line of `content` is `file: <path>`; the rest is the whole new
file. It creates the file and its directories when they do not exist. The
body stops at the next line that starts with `file: `, `<!-- file:` or
`<!-- end file content -->` (text small models copy from the prompt), and
trailing blank lines are dropped; the file ends with one newline. One file
per reply. Paths must stay in the repo and secret-bearing paths are refused,
also through a symlinked directory.

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
its exit code and output go into the role's prompt. At most 1 MiB of each
output stream (stdout, stderr) is kept.

When the limit is reached, nightshift stops the command and every process it
started (for `cargo test`: also the test program that hangs), and the role
fails with `test command timed out`. For this, the command runs in its own
process group. One effect: Ctrl-C in the terminal stops nightshift but does
not reach a test command that is running at that moment. That command ends
at its next write to its output; one that prints nothing runs until it ends
by itself.

A test command can also exit and leave a process running, for example a
server that a test started and did not stop. Nightshift does not wait for
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

Prefer this over `apply-patch` or `write-file` when the model can quote a
unique snippet but cannot write a correct diff or regenerate a large file.

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
`old:` or `new:`. The role's system prompt already explains this format,
with an example, so the role prompt does not need to repeat it.

Each `old` text must match exactly once, including overlapping occurrences;
zero matches or two or more abort the whole tool call and leave the repo
unchanged. Paths must stay inside the repo. Secret-bearing paths (the
[`context_files`](#context_files) list) are refused, also through symlinks.
The tool never creates files and never commits.

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

## Benchmark a model (`nightshift bench`)

Before a long run, check that a model works with the harness:

```bash
nightshift bench --model llama3.1:8b                       # built-in ollama
nightshift bench --provider mlx --model gemma-4-31b-4bit   # [providers.mlx] from nightshift.toml
nightshift bench --provider mlx --model qwen3.8-27b-4bit --config mac.toml --runs 5 --timeout 300
```

The client is built the same way a run builds it, so `--provider` takes
`ollama` (default) or any `[providers.*]` name from the config (`--config`,
default `nightshift.toml` when it exists). That lets you compare models on
each machine without editing roles.

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
pointed at another checkout counts as a different job. The queue's
`--config` flag is a normal command-line path. Each job writes its own run
directory under `--out`, exactly like `harness`.

Keep `--out` outside the job repos, or add it to their `.gitignore`.
Otherwise the queue's own files make the repo dirty and its job is blocked.

| Rule | What happens |
| :--- | :--- |
| One job at a time | Jobs never run in parallel. |
| A job fails | The queue moves on to the next job. `--stop-on-failure` stops instead. |
| The job's repo has uncommitted changes | The job is skipped as `blocked: dirty_tree`. Nightshift never commits, so a second job on the same checkout is blocked whenever an earlier job left changes there. Jobs that edit code need their own checkout; jobs that change nothing (a review-only graph) can share one. |
| The process is killed or the machine reboots | Run the same command again. Jobs already `done` are skipped; every other job is tried again. A job that had already changed its repo is then `blocked: dirty_tree`: review that checkout, commit or restore it, and run the command once more. A `done` job is skipped only while its entry in the queue file is unchanged (same position, goal, repo, name, config and profile). |
| Two queues use the same `--out` | The second one stops with an error. A lock file (`queue.lock`) allows one queue per `--out`; the lock is released when the process ends, even after a crash. |

Three files under `--out` track the queue:

- `queue_state.json`: each job's status (`pending`, `running`, `done`,
  `blocked`, `failed`), the reason, and its run directory. Delete it to run
  every job again. Use one `--out` per queue file.
- `queue_report.md`: the same as a table, rewritten after every job.
- `queue.lock`: the lock described above. It is safe to leave in place.

The command prints the report at the end and exits 0 only when every job is
`done`; otherwise it exits 1.
