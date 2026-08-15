# rs-nightshift

Rust CLI harness that runs one unattended software-engineering job against a
local Ollama server. The operator starts `nightshift` on the **server** (tmux or
systemd), disconnects, and reviews the working-tree diff in the morning.

The pipeline never commits.

## Status

This repository is being built in stacked PRs. The first slice is
`nightshift doctor`: it checks whether the server has Rust, Ollama, the required
models, `codegraph`, and `graphify`.

## Commands

```text
nightshift doctor
nightshift status [--out DIR]
nightshift run --goal TEXT --repo PATH --until pm|tech-lead|dev [--name SLUG] [--out DIR] [--allow-dirty]
```

`doctor` exits `0` if the environment is ready, `2` if a required check failed.

`status` prints `PASSED`, `FAILED`, or `REQUIRES_HUMAN_REVIEW` from
`./artifacts/latest/04_qa_report.json`. It exits `2` when no QA report exists.

`run --until pm` writes `01_user_story.md` with Problem Statement, User Stories,
Acceptance Criteria, and Out of Scope. `--until tech-lead` also writes
`02_tech_spec.md` from `codegraph` (and `graphify query` when a graph exists).
One `llama3.2:3b` repair is attempted if validation fails. Spec paths must be a
subset of tool output. Missing `graphify` is a warning, not a hard fail.
`--until dev` writes `03_diff.patch`, runs `git apply --check`, then applies.
The target tree becomes dirty; the pipeline never commits. Dirty trees need
`--allow-dirty`.

## Development

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo llvm-cov --workspace --fail-under-lines 85
```

Pull requests are reviewed by [rs-guard](https://github.com/nebulaideas/rs-guard)
using [`.github/review-prompt.md`](.github/review-prompt.md). Set the
`DEEPSEEK_API_KEY` repository secret so the review workflow can post.

## License

MIT
