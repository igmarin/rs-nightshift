# Development

## Required tools

The project uses [mise](https://mise.en.dev) for dev tools. With `.mise.toml`
in the repo, run:

```text
mise install
```

This installs Rust, `cargo-binstall`, `cargo-audit`, `cargo-deny`, and
`cargo-llvm-cov` (along with `fd` and `gum`). If you do not use mise, install
the tools manually:

```text
cargo install cargo-audit --version 0.22.2 --locked
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-llvm-cov --version 0.6.13 --locked
```

or, if you have `cargo-binstall`:

```text
cargo binstall cargo-audit@0.22.2 cargo-deny@0.20.2 cargo-llvm-cov@0.6.13 -y
```

## Local gates

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --doc
cargo audit
cargo deny check
cargo llvm-cov --workspace --fail-under-lines 85
```

All gates must pass before a PR is marked ready for review. CI runs the same set
plus the release build smoke test and `actionlint`.

## Pre-push gate

For a quick pre-push check, run the gate subset:

```text
./scripts/pre-push-gate.sh
```

or, equivalently:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --doc
cargo audit
cargo deny check
```

The full local gate (including coverage) is in the [Local gates](#local-gates) section above.

## PR workflow

1. Open the PR as a **draft**. CI runs on draft PRs — let all gates go green
   while it's still draft.
2. Do **not** mark "Ready for review" until CI is fully green. The ocr review
   runs on non-draft PRs, so marking ready is what starts it.
3. Merge only when CI is green **and** every `critical` or `high` ocr finding
   is fixed or answered.

## ocr review

Pull requests are reviewed by
[ocr](https://github.com/alibaba/open-code-review) (Claude Haiku, high effort)
using [`.github/review-prompt.md`](../.github/review-prompt.md) as background.
Findings post inline as the `nebula-rs-guard` bot. Set the `CLAUDE_API_KEY` and
`GH_PAT` repository secrets so the workflow can post. Any `critical` or `high`
finding blocks merge; resolve it, push, and the review reruns.

Locally, `git config core.hooksPath .githooks` enables a pre-commit hook that
runs `ocr review` on staged changes and blocks on critical/high findings
(needs `CLAUDE_API_KEY` in your shell and
`npm i -g @alibaba-group/open-code-review`; without them it skips;
`git commit --no-verify` skips it too).

## CI gates

CI (`.github/workflows/ci.yml`) runs on pushes to `main` and pull requests
targeting `main`:

- Format check (`cargo fmt --all -- --check`)
- Clippy (`cargo clippy --all-targets --all-features -- -D warnings`)
- Tests (`cargo test`)
- Doc tests (`cargo test --doc`)
- `cargo-deny` (license and advisory checks)
- `cargo-audit` (security vulnerability scan)
- Line coverage (`cargo llvm-cov --workspace --fail-under-lines 85`)
- Release build smoke test
- `actionlint` (workflow YAML linting)
- Toolchain consistency (rust-toolchain.toml vs ci.yml vs release-checks.yml)

## Release checks

`.github/workflows/release-checks.yml` is a reusable workflow that mirrors the
CI quality gates, pinned to the crate's `rust-version`. It runs before any
release artifacts are built. The drift-detection test
(`tests/workflow_consistency.rs`) ensures it stays in sync with `ci.yml`.
