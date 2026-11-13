# AGENTS.md

Instructions for coding agents (Claude Code, Codex, Cline, or a nightshift
role that lists this file in `context_files`) working on rs-nightshift.
`CLAUDE.md` is a link to this file.

rs-nightshift is a Rust CLI, `nightshift`, that runs coding jobs on local
models: it walks a graph of roles declared in `nightshift.toml`, applies the
edits the models write, and runs the repo's tests. See the
[README](README.md).

## Read first

1. [`docs/architecture.md`](docs/architecture.md): how the code is organized.
2. [`docs/decisions.md`](docs/decisions.md): what is decided and why. Don't
   reopen a decision without a new reason.
3. [`.github/review-prompt.md`](.github/review-prompt.md): the invariants
   INV-1 to INV-11 that every PR is reviewed against. A change that breaks one
   is rejected. The ones that bite most often:
   - The pipeline never runs `git commit`, `push`, `reset --hard` or `clean`
     on the target repo.
   - Library code has no `unwrap`, `expect`, `panic!` or `process::exit`.
   - Model names live in config, never in code: the same graph runs on a Mac
     with rapid-mlx and on a Linux box with Ollama.
   - Line coverage stays at or above 85%.

## Build and test

Install the tools once with [mise](https://mise.en.dev): `mise install`
(Rust, `cargo-audit`, `cargo-deny`, `cargo-llvm-cov`). Without mise, see
[`docs/development.md`](docs/development.md#required-tools).

The local gate, the same checks CI runs:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --doc
cargo doc --no-deps
cargo audit
cargo deny check
cargo llvm-cov --workspace --fail-under-lines 85
```

`./scripts/pre-push-gate.sh` runs all of them except coverage. CI also runs a
release build, `actionlint` and `scripts/check-action-pins.sh`
([CI gates](docs/development.md#ci-gates)).

## Git hooks

Enable both hooks once:

```text
git config core.hooksPath .githooks
```

- `pre-commit` runs an `ocr review` of the staged changes and blocks the
  commit on critical or high findings. It sends the staged diff to OpenAI's
  API, and needs `OPENAI_API_KEY` and
  `npm i -g @alibaba-group/open-code-review`; without them it skips.
- `pre-push` runs `scripts/pre-push-gate.sh`.

Agents don't skip either hook with `--no-verify`. If a hook can't run on your
machine, say so in the PR.

## Branch and PR flow

1. Branch from `main`. One concern per PR; aim for under 150 changed lines.
2. Write the test first. A bug fix starts with a test that fails.
3. Run the local gate. Update `CHANGELOG.md` under `[Unreleased]` for any
   change a user can see, and the docs that describe it.
4. Open the PR as a draft, using
   [the PR template](.github/pull_request_template.md). CI runs on drafts.
5. When CI is green, mark it ready: `gh pr ready`. That starts the ocr review
   (OpenAI `gpt-6-luna`, findings posted inline by `github-actions[bot]`).
6. Fix or answer every critical and high finding, reply on each thread, and
   resolve it. After pushing fixes, review the new head with
   `gh pr ready --undo && gh pr ready`; re-running the old job reviews the
   old commit.
7. The owner merges (squash). Agents never merge.

Rules for every branch:

- Never force-push. To catch up with `main`, merge `origin/main` into the
  branch.
- Write `Closes #N` only when the PR finishes the issue. Otherwise write
  `Part of #N`: GitHub closes the issue for any "close", "fix" or "resolve"
  right before `#N`, even in a sentence like "does not close #N".

## Local files

`graphify-out/` and `.codegraph/` are code indexes built on each machine
(`graphify .`, `codegraph init`). They are gitignored and never committed. If
`.codegraph/` exists, `codegraph explore "<query>"` answers questions about
symbols and callers faster than grep.

`artifacts/` holds local nightshift runs and is gitignored too.
