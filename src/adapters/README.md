# src/adapters — the I/O layer

The adapters implement the ports (ADR-007): this is the only layer allowed to
import `llm-kernel` / `reqwest` or shell out to external tools. Everything the
application needs from the outside world enters through here, behind the port
traits in `ports.rs`.

| File              | Implements                | What it does |
| :---------------- | :------------------------ | :----------- |
| `mod.rs`          | `ModelClient`, `ModelClientFactory` | the LLM provider adapters (`OllamaAdapter`, `OpenAICompatibleAdapter`) and the `build_model_client` / `ProviderFactory` wiring — see [`providers/README.md`](providers/README.md) |
| `capabilities.rs` | `ToolRunner`, `ContextProvider` | `CapabilityRunner` (`run-tests`, `apply-patch`, `write-file`, `search-replace`) and `GraphContextProvider` (codegraph / graphify context) |
| `context.rs`      | —                        | codegraph / graphify context probe (`PathProbe`, `ContextProbe`, `gather`, `ContextBundle`) plus `extract_paths` and `path_allowed` |
| `artifact_store.rs` | `ArtifactStore`          | `FsArtifactStore` — creates `{date}_{slug}` run directories under a root (default `./artifacts`) and reads/writes artifacts inside them |
| `state.rs`        | `StateStore`              | `FsStateStore` — append-only `actions.jsonl` action log plus a `state.json` snapshot (written via temp-file + rename so a crash never truncates it) |
| `clock.rs`        | `Clock`                   | `SystemClock` — UTC time via the Unix `date` command (this adapter is allowed to shell out) |

Blocking process I/O (test runs, `git apply`, context gathering, `date`) runs
on `tokio` blocking threads so the executor never holds a blocking primitive
across `.await`.

Who owns what:

- **The adapters own the outside world** — which provider speaks which
  protocol, how a test command is detected and run, how a patch is validated
  and applied, how artifacts and state land on disk, what the current time is.
- **The adapters own safety details at the boundary** — URL validation and
  redaction, the `keep_alive: 0` VRAM unload, `git apply --check` before
  applying, and the per-call completion timeout.

Rules that hold here:

- Domain and application never import from this layer — dependencies point
  *inward* (adapters depend on `ports` and `domain` types, never the reverse).
- The CLI edge (`main.rs`) constructs the adapters and injects them; nothing
  in the application layer mentions them by name.
