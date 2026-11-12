# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Added

- `write-file` creates the file, and its parent directories, when it does
  not exist yet. Before, it could only replace an existing file, so no tool
  could add a new file without a hand-written diff. The path rules are the
  same: inside the repo, no secret-bearing paths, symlinks resolved.
- macOS unattended runs: `contrib/dev.nightshift.queue.plist` (a launchd
  agent that waits for the model server, then runs a goal queue under
  `caffeinate -i`),
  `contrib/dev.nightshift.model-server.plist` (keeps a local model server
  up), and `docs/macos.md`. `nightshift doctor` adds a non-blocking
  `caffeinate` check on macOS (#112).
- `nightshift queue --file goals.toml [--config PATH] [--out DIR]
  [--stop-on-failure]` runs many harness jobs back to back. A failed job does
  not stop the queue, a job whose repo has uncommitted changes is skipped as
  `blocked: dirty_tree`, and a restart skips the jobs already done. Progress
  is kept in `queue_state.json` and `queue_report.md` under `--out` (#110).
- `nightshift doctor` warns (`num_ctx:<role>`) when a role sets the `num_ctx`
  option: no adapter sends it, so the context size must be set in the model
  itself (Ollama: `PARAMETER num_ctx` in a Modelfile).
- Per-role `retries` (default 2): when a reply is not a valid envelope, an
  edit tool (`apply-patch`, `write-file`, `search-replace`) fails, or the
  provider request fails, the role tries again instead of failing the run.
  Rejected replies get the error as feedback, each retry adds a `retry`
  event to `actions.jsonl`, and every rejected reply is kept as
  `raw_response_<role>_<attempt>.txt` (was `raw_response_<role>.txt`).
  Timeouts are not retried. `retries = 0` keeps the old behavior (#124).
- Machine profiles: a role can use `model = "@slot"`, and `[profiles.<name>]`
  maps slots to `{ provider, model, options }`. Select with `--profile`,
  `NIGHTSHIFT_PROFILE`, or `[run] profile`. `doctor`, `harness`, and `plan`
  show the resolved models; `bench --model @slot` benchmarks a slot (#108).
- Keyless local OpenAI-compatible providers (rapid-mlx, llama.cpp, LM
  Studio): `api_key_env` is optional for custom `openai-compatible`
  providers. `nightshift doctor` now checks that each such provider is
  reachable (`GET {base_url}/models`) and serves every model a role names
  (`model:<provider>:<model>`), listing the served models when one is
  missing (#109).
- `nightshift bench --model <tag> [--provider <name>] [--runs N] [--timeout S]`
  checks a model against the harness: role-envelope JSON, exact quoting, and
  a search-replace block, each repeated with a pass rate and latency. Works
  with any provider (Ollama, rapid-mlx, any `[providers.*]`) (#100).

### Changed

- A run never reuses another run's directory: when `{date}_{slug}` already
  exists (two runs started in the same second), `-2`, `-3`, … is appended.
- CI and release checks now run `cargo doc --no-deps`, and broken or
  private doc links fail the build. Removed the stale `docs/plan-0.1.0.md`
  handoff note. The crate description now names OpenAI-compatible servers.
- Provider error messages no longer say "Ollama" for every provider: they
  now read `provider request failed: …`, `provider request timed out` and
  `model not found: …`, since the same errors come from rapid-mlx and other
  OpenAI-compatible servers.
- OpenAI-compatible `base_url` values with a query string or fragment are
  rejected: endpoints are appended to the base URL, so they would hit the
  wrong path.
- OpenAI-compatible providers may use plain `http://` only on a loopback host,
  and may omit `api_key_env` only on a loopback host.
- Per-role `timeout_secs` in `nightshift.toml`. Omitted roles use the 3600s
  generate default; `timeout_secs = 0` is a config error. See issue
  [#101](https://github.com/igmarin/rs-nightshift/issues/101).
- `search-replace` capability: roles can declare `tools = ["search-replace"]`
  and return `file:` / `old:` / `new:` blocks. Each `old` snippet must match
  exactly once; not-found and ambiguous matches abort with no writes. See
  issue [#97](https://github.com/igmarin/rs-nightshift/issues/97).

### Fixed

- A reply with prose or a code listing before its JSON envelope is accepted.
  The parser took the first `{` in the reply, so braces in that text hid a
  valid envelope and the reply was rejected with a JSON syntax error. A reply
  that opens with prose and has no envelope is now rejected as "model did
  not return a JSON object".
- `write-file` ends the file with a newline. It used to drop the final
  newline, so every file it wrote failed format checks.
- `search-replace` restores the files it already wrote when a later write
  fails, so a failed call leaves the repo unchanged and is safe to retry.
- `search-replace` roles: the contract now tells the model to keep the whole
  edit inside the one `content` JSON string. Local models closed the string
  before `new:`, which ended the run with "did not return a JSON object".
  `nightshift bench` now tests that same path (the role contract plus the
  envelope, with a generic role sentence), and `text-quoting` is
  informational (`WARN`), so the bench verdict is closer to what a real role
  does (#123).
- Context gathering runs `codegraph init` when `.codegraph/` exists but has
  no `codegraph.db` (for example, a repo that commits `.codegraph/.gitignore`).
  See issue [#119](https://github.com/igmarin/rs-nightshift/issues/119).
- Role JSON parsing strips `<think>…</think>` reasoning from thinking models
  (`qwen3`, `deepseek-r1`) before extracting the envelope. See issue
  [#98](https://github.com/igmarin/rs-nightshift/issues/98).

## [0.1.0]

### Added

- An unattended pipeline running PM, Tech Lead, Dev, QA, and optional Writer stages.
- `doctor`, `status`, and `run` commands for checking the server, reading QA
  status, and running a job.
- Dated run artifacts containing the user story, tech spec, applied diff, QA
  report, optional article draft, pipeline state, and log.
- Prebuilt binaries for `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`,
  `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`.
- Configurable Ollama origin via `--ollama-url` flag or
  `NIGHTSHIFT_OLLAMA_URL` environment variable (defaults to
  `http://127.0.0.1:11434`). Invalid URLs are rejected by `doctor` and `run`.
- Configurable role-to-model mapping via `nightshift.toml` `[role_models]`
  table, with `NIGHTSHIFT_CONFIG` environment variable for the file path.
  Defaults work without any config file.
- `nightshift.toml.example` at repo root with all options documented.
- Configuration section in README with precedence rules, env vars, and
  role-to-model default table.
- `doctor` check for `nightshift.toml` config file (reports overrides or
  parse errors as a non-required check).
- Drift-detection test (`tests/workflow_consistency.rs`) that asserts
  `release-checks.yml` runs a superset of CI quality gates.

### Changed

- Split `doctor.rs` into `doctor/{mod,catalog,host,report}.rs` submodules
  for better cohesion (graphify-measured improvement).
- Split `artifacts.rs` into `artifacts/{mod,state,qa,util}.rs` submodules
  for better cohesion.
- Deduplicated `ContextProbe` test mocks in `pipeline.rs` into a single
  `StubProbe`.

### Fixed

- `redact_ollama_url` now strips the entire userinfo (username + password)
  from URLs, not just the password.
- `validate_ollama_url` rejects URLs containing userinfo entirely.
- Least-privilege permissions in `release.yml` (replaced invalid
  `permissions = null` with job-level `contents: read` / `contents: write`).
- Added `cargo test --doc` to `release-checks.yml` (was missing from the
  release gate, found by drift-detection test).
- Raised rs-guard review token budget so reviews on medium diffs produce
  a verdict instead of empty-content failures.
