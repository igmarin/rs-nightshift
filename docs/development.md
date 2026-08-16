# Development

## Local gates

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --doc
cargo llvm-cov --workspace --fail-under-lines 85
```

All five must pass before a PR is marked ready for review. CI runs the same set
plus `cargo-deny`, `cargo-audit`, and `actionlint`.

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

CI (`.github/workflows/ci.yml`) runs on every push and pull request:

- Format check (`cargo fmt --all -- --check`)
- Clippy (`cargo clippy --all-targets --all-features -- -D warnings`)
- Tests (`cargo test`)
- Doc tests (`cargo test --doc`)
- Line coverage (`cargo llvm-cov --workspace --fail-under-lines 85`)
- Release build smoke test
- `cargo-deny` (license and advisory checks)
- `cargo-audit` (security vulnerability scan)
- `actionlint` (workflow YAML linting)
- Toolchain consistency (rust-toolchain.toml vs ci.yml vs release-checks.yml)

## Release checks

`.github/workflows/release-checks.yml` is a reusable workflow that mirrors the
CI quality gates, pinned to the crate's `rust-version`. It runs before any
release artifacts are built. The drift-detection test
(`tests/workflow_consistency.rs`) ensures it stays in sync with `ci.yml`.
