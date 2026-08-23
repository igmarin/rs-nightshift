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
