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
| `text-quoting` | is the two requested lines, exact (the first line may drop some of its leading spaces), with no fences or prose |
| `search-replace` | is one `file:`/`old:`/`new:` block that, applied to the sample file, gives the expected file (nothing is written to disk) |

Output shows the pass rate and the average and max latency per task. The
first call can be slow while the server loads the model. The command exits
0 only when every run of every task passes:

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

## Role tools

Each `[[roles]]` block may declare `tools = [...]`. Models return text plus a
verdict; the harness performs the side effect. Unknown names are rejected at
config load.

| Tool | When it runs | What it does |
| :--- | :----------- | :----------- |
| `gather-context` | Before the LLM call | Injects codegraph/graphify context (and optional `context_files`) into the prompt |
| `run-tests` | Before the LLM call | Runs the detected test command and injects the results |
| `apply-patch` | After `continue` / `done` | Applies the role's `content` as a unified diff (`git apply --check`, then apply) |
| `write-file` | After `continue` / `done` | Writes `content` that starts with `file: <path>` as the full file |
| `search-replace` | After `continue` / `done` | Replaces unique `old:` snippets with `new:` text in existing files |

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
`file:` header) are allowed. Each `old` text must match exactly once, including overlapping occurrences;
zero matches or two-plus matches abort the whole tool call and leave the repo
unchanged. Paths must stay inside the repo. Secret-bearing paths (`.git`,
`.env`, keys, and the rest of the `context_files` denylist) are rejected,
including symlinks that resolve into those paths. The tool never creates
files and never commits.
