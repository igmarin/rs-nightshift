# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Added

- `nightshift bench --model <tag>`: three fast harness-compatibility
  micro-tasks (JSON validity, text quoting, instruction-following format)
  so operators can reject a thinking or too-small model in minutes instead
  of a 30–60 minute CPU run. See issue
  [#100](https://github.com/igmarin/rs-nightshift/issues/100).
- `search-replace` capability: roles can declare `tools = ["search-replace"]`
  and return `file:` / `old:` / `new:` blocks. Each `old` snippet must match
  exactly once; not-found and ambiguous matches abort with no writes. See
  issue [#97](https://github.com/igmarin/rs-nightshift/issues/97).

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
