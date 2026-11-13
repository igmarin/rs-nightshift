# src/adapters: the I/O layer

The adapters implement the ports (ADR-007). In the role-graph layers, this
is the only place that imports `llm-kernel` or `reqwest` or starts other
programs. Everything the application needs from outside comes in through
here, behind the port traits in `ports.rs`.

| File | Implements | What it does |
| :--- | :--- | :--- |
| `mod.rs` | | declares the modules and re-exports the provider adapters and `build_model_client` / `ProviderFactory` |
| `providers/` | `ModelClient`, `ModelClientFactory` | the model provider adapters (`OllamaAdapter`, `OpenAICompatibleAdapter`) and the factory; see [`providers/README.md`](providers/README.md) |
| `ollama.rs` | | `OllamaClient` (completion, then the `keep_alive: 0` unload), Ollama URL checks and redaction, `DEFAULT_GENERATE_TIMEOUT` |
| `kernel_error.rs` | | maps llm-kernel errors to `ProviderError` (`map_kernel_error`), rejects a reply cut off at the output limit (`complete_text`), and reads the token counts (`usage`) |
| `capabilities.rs` | `ToolRunner`, `ContextProvider` | `CapabilityRunner` (`run-tests`, `apply-patch`, `write-file`, `search-replace`) and `GraphContextProvider` (codegraph and graphify context) |
| `context.rs` | | the codegraph and graphify probe (`PathProbe`, `ContextProbe`, `gather`, `ContextBundle`), plus `extract_paths` and `path_allowed` |
| `git.rs` | | `apply_checked` (path checks, `git apply --check`, then a hunk-header repair), `working_tree_dirty`, `head_commit` |
| `test.rs` | | test-command detection (`detect_test_command`) and `ProcessTestRunner`: runs the command in its own process group with a 600 s limit and keeps up to 1 MiB of each output stream |
| `artifact_store.rs` | `ArtifactStore` | `FsArtifactStore`: creates run directories under a root (default `./artifacts`) and reads and writes artifacts in them |
| `state.rs` | `StateStore` | `FsStateStore`: the append-only `actions.jsonl` and the `state.json` snapshot, written through a temp file and a rename so a crash never truncates it |
| `clock.rs` | `Clock` | `SystemClock`: runs the Unix `date` command for UTC timestamps and the local date |

Blocking process I/O (test runs, `git apply`, context gathering, `date`)
runs on `tokio` blocking threads, so the executor never holds a blocking
call across `.await`.

The adapters also own the safety checks at the edge: URL validation and
redaction, the Ollama unload, the patch path checks before `git apply`, the
secret-path rules for the edit tools, and the per-call completion timeout.

Dependencies point inward: adapters use `ports` and `domain` types, and the
domain and application never import from this layer. `main.rs` builds the
adapters and passes them in.
