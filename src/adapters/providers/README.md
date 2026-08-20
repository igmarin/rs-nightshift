# src/adapters/providers — the ModelClient provider adapters

> **Location note:** in this branch the provider code is implemented in
> `src/adapters/mod.rs` (see its module-level docs). A `providers/` submodule
> split (e.g. `ollama.rs`, `openai.rs`, `options.rs`, `factory.rs`,
> `test_support.rs`) exists on the `refactor-adapters` branch; this README
> documents the layer so its home does not depend on where the code lands.

The provider layer turns a role's `provider` + `ProviderSpec` + `options` into
a concrete `ModelClient`, which the application consumes through the
`ModelClient` / `ModelClientFactory` ports. It is the only place that talks to
an LLM endpoint.

## Adapters

- **`OllamaAdapter`** — wraps `OllamaClient` (top-level `ollama.rs`),
  preserving its `keep_alive: 0` VRAM-unload after each call. With
  `options.think = true` the `:think` model-tag suffix is appended (`phi4` →
  `phi4:think`); a tag that already ends in `:think` is left untouched. Local,
  no API key (a placeholder is passed to `llm-kernel`'s OpenAI client).
- **`OpenAICompatibleAdapter`** — wraps `llm-kernel`'s `OpenAIClient` and is
  used for `deepseek`, `kimi`, and any custom `openai-compatible` provider.
  The base URL must be `http(s)://` with a host and no embedded credentials
  (rejected values are reported redacted); completions are wrapped in
  `tokio::time::timeout` so a hanging provider surfaces as `Error::Timeout`.
  Kernel errors are mapped via `map_kernel_error` (e.g. a 404 becomes
  `Error::ModelNotFound`).

## Factory

`build_model_client` (exposed through `ProviderFactory`, the
`ModelClientFactory` port implementation) resolves the provider name:

| Provider  | Backend             | Default base URL                | API key env var   |
| :-------- | :------------------ | :------------------------------ | :---------------- |
| `ollama`  | built-in            | `http://127.0.0.1:11434`        | —                 |
| `deepseek`| OpenAI-compatible   | `https://api.deepseek.com`      | `DEEPSEEK_API_KEY`|
| `kimi`    | OpenAI-compatible   | `https://api.moonshot.cn/v1`    | `MOONSHOT_API_KEY`|
| any other | `openai-compatible` (required) | from `[providers.<name>]` | `api_key_env`     |

A `[providers.<name>]` block overrides the defaults (`base_url`,
`api_key_env`); a custom provider must supply both. Unknown providers or
backends, missing key env vars, and malformed base URLs are configuration
errors (`Error::Config`).

## Options (`RoleSpec.options`)

- `temperature` — float or integer in `0.0..=2.0`; overrides the request
  temperature when present.
- `max_tokens` — non-negative integer; caps the completion length.
- `think` — boolean; honored by the Ollama adapter (`:think` tag suffix).
- `num_ctx` — parsed and validated but deliberately **not forwarded**:
  llm-kernel's OpenAI-compatible request has no `num_ctx` field, so the value
  cannot reach Ollama through this path.

Unknown option keys are ignored (the schema declares `options` a
provider/model-specific passthrough); invalid option *types* are config errors
so typos surface at build time rather than silently.
