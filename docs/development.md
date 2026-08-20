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
2. Do **not** mark "Ready for review" until CI is fully green. Marking ready is
   what triggers rs-guard (the review workflow runs only on `ready_for_review`).
3. Merge only when CI is green **and** the rs-guard verdict is **POSITIVE**
   (0 Critical, 0 Security, Important < 3).

## rs-guard review

Pull requests are reviewed by
[rs-guard](https://github.com/nebulaideas/rs-guard) using
[`.github/review-prompt.md`](../.github/review-prompt.md). Set the
`DEEPSEEK_API_KEY` repository secret so the review workflow can post.

rs-guard posts a structured verdict:

```text
[RS_GUARD_VERDICT_METADATA]
Verdict: POSITIVE | NEGATIVE
CriticalIssues: N
SecurityIssues: N
ImportantIssues: N
Suggestions: N
```

Any `[Critical]` or `[Security]` finding blocks merge. A `NEGATIVE` verdict
blocks merge. Resolve the findings, push, and re-request review.

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
