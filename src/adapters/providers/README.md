# src/adapters/providers: the model provider adapters

This module turns a role's `provider`, `ProviderSpec` and `options` into a
`ModelClient`, which the application uses through the `ModelClient` and
`ModelClientFactory` ports. It is the only role-graph code that talks to a
model server.

| File | Contents |
| :--- | :--- |
| `factory.rs` | `build_model_client`, `ProviderFactory` (the `ModelClientFactory` implementation), the built-in defaults |
| `ollama.rs` | `OllamaAdapter` |
| `openai.rs` | `OpenAICompatibleAdapter` and the base-URL checks |
| `options.rs` | parsing and checking a role's `options` |
| `scripted.rs`, `test_support.rs` | test helpers (`ScriptedGenerator`, wiremock setups) |

## Adapters

- `OllamaAdapter` wraps `OllamaClient` (`src/adapters/ollama.rs`), which
  sends `keep_alive: 0` after each call to unload the model (not when
  `[providers.ollama] keep_loaded = true`). With
  `options.think = true` it adds the `:think` tag suffix (`phi4` becomes
  `phi4:think`); a tag that already ends in `:think` is left as it is. Ollama
  needs no API key, so a placeholder goes to llm-kernel's client.
- `OpenAICompatibleAdapter` wraps llm-kernel's `OpenAIClient`. It serves
  `deepseek`, `kimi` and every custom `openai-compatible` provider. The base
  URL must be `http://` or `https://` with a host, with no credentials, query
  or fragment, and plain `http://` only on this machine (loopback). Errors
  show the URL with credentials removed.

Both adapters wrap the completion in `tokio::time::timeout`, so a hanging
server ends as `ProviderError::Timeout`, and map llm-kernel errors with
`map_kernel_error` (a 404 becomes `ProviderError::ModelNotFound`). A reply
cut off at the output limit is rejected (`ProviderError::CutOff`).
`generate_with_usage` also returns the token counts the server reports.

## Factory

`build_model_client` (behind `ProviderFactory`) picks the adapter by
provider name:

| Provider | Backend | Default base URL | API key env var |
| :--- | :--- | :--- | :--- |
| `ollama` | built in | `http://127.0.0.1:11434` | none |
| `deepseek` | OpenAI-compatible | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` |
| `kimi` | OpenAI-compatible | `https://api.moonshot.cn/v1` | `MOONSHOT_API_KEY` |
| any other | `openai-compatible` (required) | from `[providers.<name>]` | `api_key_env` |

A `[providers.<name>]` block overrides the defaults (`base_url`,
`api_key_env`). A custom provider must set `base_url`. It may leave out
`api_key_env` only when `base_url` is loopback (a local server such as
rapid-mlx); it then sends the placeholder key `local`. An unknown provider
or backend, a missing key variable, and a bad base URL are config errors
(`Error::Config`).

## Options (`RoleSpec.options`)

- `temperature`: a number from 0.0 to 2.0; overrides the default (0.2).
- `max_tokens`: a non-negative integer; caps the reply length.
- `think`: a boolean; only the Ollama adapter uses it (the `:think`
  suffix).
- `num_ctx`: checked but not sent. llm-kernel's OpenAI-compatible request
  has no `num_ctx` field, so the value cannot reach Ollama this way.
  `nightshift doctor` warns when a role sets it.

Unknown keys are ignored, because `options` passes provider and model
settings through. A wrong type is a config error. It is raised when the
role's client is built, as the role starts, so a typo in a value stops the
run instead of being ignored.
