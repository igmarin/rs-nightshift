Repo: `github.com/igmarin/rs-nightshift` (local main dir `/home/igmarin/Work/rs-nightshift`). `gh` is authenticated as `igmarin` — never paste or log a token.

## Status snapshot

The main rework is **done and merged**: `main` (`01ecd84`) is the config-driven role-graph harness (PR #75). The current work is a series of **small post-merge refactor slices** (adapters cleanup) plus docs, run as **parallel git worktrees** (coding-flow style). The objective still open is: finish relocating the legacy reusable primitives into the adapters layer so #74 (retire the old pipeline) becomes a clean deletion.

## Active branches / worktrees

| Path | Branch | Purpose |
|---|---|---|
| `/home/igmarin/Work/rs-nightshift` | `main` | main worktree (config-driven role graph merged) |
| `/home/igmarin/Work/rs-nightshift-primitives` | `refactor-primitives` | relocate primitives (slices 2-5, **stacked on main**) |
| `/home/igmarin/Work/rs-nightshift-docs` | `docs-architecture` | docs (slices 3+4) |

## Open issues (board)

- **#76** relocate reusable primitives into `adapters/` — **in progress**, move 2/5 (git) done, continue moves 3/5-5/5.
- **#82** refactor global `Error` enum into domain/adapter-specific errors — do after #76, before #74.
- **#83** consolidate `ScriptedGenerator` with `adapters/providers` LLM client factory — parallel with #82.
- **#84** add `Error` decoupling milestone between #76 and #74 — tracks the sequencing.
- **#85** add `cargo-audit` and `cargo-deny` to dev/CI environment — parallel tooling.
- **#74** retire hardcoded stage modules + `Role` enum — blocked by #76, #82, #83.
- **#36** beta acceptance run (external — needs the beta machine + live Ollama/keys).
- **#17** cut v0.1.0 release.
- Deferred review follow-ups (tracked, not blocking): client caching across graph steps; persist terminal `failed` snapshot on error; bound artifact retention; inject env resolver instead of `EnvGuard`.

## Next concrete step

Continue **issue #76, move 3/5**, on branch `refactor-primitives` (worktree `rs-nightshift-primitives`):

Relocate the test runner primitives from `src/testrun.rs` → `src/adapters/test.rs`. Pure move, **no behavior change**; run the full gate after.

Then move 4/5 (`context.rs` → `adapters/context.rs`) and 5/5 (`ollama.rs` → `adapters/ollama.rs`), each its own commit stacked on the `refactor-primitives` branch.

After #76 finishes, work on **#82** (split `Error`) and **#83** (consolidate `ScriptedGenerator`) before returning to **#74**.

## Order of work

1. **#76** — finish adapter relocations (testrun, context, ollama). [in progress]
2. **#82** + **#83** — split the monolithic `Error` enum and consolidate `ScriptedGenerator` into adapters/providers. Can run in parallel after #76.
3. **#74** — retire hardcoded stage modules + `Role` enum, unblocked once #82 and #83 land.
4. **#85** — add `cargo-audit` and `cargo-deny` to dev/CI. Parallel tooling improvement.
5. **#36** — beta acceptance run.
6. **#17** — cut v0.1.0 release.

## Workflow conventions (non-negotiable)

- **coding-flow**: one worktree per slice; ticket → branch → PR; stack with `git worktree` (no `herdr`).
- **Full pre-push gate** (run every time): `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`. `cargo audit` and `cargo deny check` become required after `#85` provisions them.
- **refactor-process**: baseline green → one atomic move → run tests → commit; rollback immediately on red; never mix behavior changes.
- Repo invariants (see PR boilerplate / `docs/development.md`): no `unwrap`/`expect`/`panic!` outside `#[cfg(test)]`, `#![deny(missing_docs)]`, model output never used as argv, never commit/push/reset/clean.

## Architecture (one line)

Hexagonal (ADR-007): `src/domain/rolegraph/` (pure), `src/ports.rs` (traits + test doubles), `src/application/` (executor/orchestrator/report — no I/O), `src/adapters/` (the only I/O layer: `providers/`, `capabilities`, `artifact_store`, `state`, `clock`, `kernel_error`). Legacy modules (`pipeline`, `pm`, `techlead`, `qa`, `writer`, `models`, `dev`, `context`, `generate`, `testrun`, `ollama`, `artifacts`, `doctor`) still coexist, retiring via #74.

## Reference artifacts (do not duplicate)

- `docs/role-graph.md` — config schema + hexagonal design (source of truth).
- `docs/architecture.md` — rewritten for the harness.
- `docs/decisions.md` — ADR-006 (role graph), ADR-007 (hexagonal).
- `src/**/README.md` — per-folder ownership (added in PR #79).
- `nightshift.toml.example` — new-schema example.

## Suggested skills (call the Skill tool)

- `coding-flow` — worktree/PR orchestration.
- `refactor-process` — the atomic relocation moves.
- `review-domain-boundaries` / `review-architecture` — structure checks.
- `respond-to-review` — when triaging CI/rs-guard/CodeRabbit comments.
- `graphify` / `codegraph` (CLI) — code-picture queries.
- `github-issue` — board updates.
- `tdd-process` — if any behavior is added.

