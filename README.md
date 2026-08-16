# rs-nightshift

Rust CLI that runs **one** unattended software-engineering job on a **server**.
The operator SSHs in, starts `nightshift` under tmux or systemd, and disconnects.
In the morning they review the dirty working tree in Zed (or a terminal) and
either commit or restore.

The pipeline never commits, pushes, resets, or cleans.

## Install

Prebuilt `nightshift` binaries are published on every `vX.Y.Z` tag for Linux
(`x86_64`, `aarch64`) and macOS (`x86_64`, `aarch64`):

```text
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/igmarin/rs-nightshift/releases/latest/download/rs-nightshift-installer.sh | sh
```

The script picks the tarball for the host triple, verifies the published
`sha256` checksum, and installs `nightshift` on `PATH` (`$CARGO_HOME/bin`,
usually `~/.cargo/bin`; add it to `PATH` if your shell does not already). Each
release also carries per-target `.tar.xz` archives and a `sha256.sum` file for
manual installs.

The binary is portable, but a full `run` still needs a prepared server: Ollama
with the role models plus `codegraph` and `graphify` on `PATH` — see
[Prerequisites (server)](#prerequisites-server) and run `nightshift doctor`
after installing.

## Server vs laptop

```text
[laptop]  SSH start  →  [server: tmux or systemd]
                         nightshift run --goal … --repo …
                         (operator disconnects; process keeps running)
[laptop]  SSH / Zed  ←  morning: status, git diff, commit or restore
```

`nightshift` does not daemonize itself.

## Prerequisites (server)

- Rust (mise or rustup)
- Ollama listening on `127.0.0.1:11434` with these tags:
  `llama3.2:3b`, `llama3.1:8b`, `mistral-nemo:12b`, `qwen2.5-coder:7b`,
  `deepseek-r1:7b`, `gemma2:9b`, `phi3.5:latest`
- `codegraph` and `graphify` on `PATH`
- Pre-build `.codegraph/` in the target repo (`codegraph init`). Optional:
  `graphify-out/graph.json` for Tech Lead context. Nightshift will `codegraph
  init` once if the index is missing. It never rebuilds a full graphify corpus.

```text
nightshift doctor [--ollama-url URL]
```

`doctor` exits `0` if the environment is ready, `2` if a required check failed.

## Configuration

All settings are optional — built-in defaults work without any configuration
file. Precedence: **CLI flag > environment variable > `nightshift.toml` > built-in default**.

### Ollama origin

| Key | CLI flag | Env var | Default |
| :--- | :--- | :--- | :--- |
| Ollama URL | `--ollama-url` | `NIGHTSHIFT_OLLAMA_URL` | `http://127.0.0.1:11434` |

Set via CLI flag or env var (not read from `nightshift.toml`). Invalid URLs
are rejected by `doctor` and `run`.

### `nightshift.toml`

| Key | Env var | Default |
| :--- | :--- | :--- |
| File path | `NIGHTSHIFT_CONFIG` | `nightshift.toml` |
| `[role_models]` table | — | built-in defaults (see below) |

A sample file is at [`nightshift.toml.example`](nightshift.toml.example). Copy
it to `nightshift.toml` in the directory where you run `nightshift`:

```text
cp nightshift.toml.example nightshift.toml
```

### Role-to-model mapping

Override the default model for any role in `nightshift.toml`:

```toml
[role_models]
Dev = "qwen2.5-coder:14b"
Qa = "deepseek-r1:14b"
```

| Role | Default model | Responsibility |
| :--- | :--- | :--- |
| Router | `llama3.2:3b` | Fast schema repair and payload checks |
| Pm | `llama3.1:8b` | User-story writer |
| TechLead | `mistral-nemo:12b` | Architect / tech spec |
| Dev | `qwen2.5-coder:7b` | Implementation / patch author |
| Qa | `deepseek-r1:7b` | Test-failure reasoner |
| Writer | `gemma2:9b` | Changelog and article writer |
| Aux | `phi3.5:latest` | Lightweight sanity check |

## Commands

```text
nightshift doctor
nightshift status [--out DIR]
nightshift run --goal TEXT --repo PATH [--ollama-url URL] [--name SLUG] [--out DIR] [--allow-dirty] [--article|--no-article] [--until pm|tech-lead|dev|qa]
```

Omit `--until` for a full run: PM → Tech Lead → Dev apply → QA (max 3) → Writer
(if `--article`, default on, and QA `PASSED`).

Global `--ollama-url` is accepted after `doctor`, reads `NIGHTSHIFT_OLLAMA_URL`, and defaults to `http://127.0.0.1:11434`; the flag takes precedence. Invalid URLs are reported by `doctor` as failed required checks and exit with code `2`.

`--until` is a debug stop. `--allow-dirty` is required when the target tree is
already dirty. The test argv comes from `nightshift.toml` or a detector
(`cargo test`, `bundle exec rspec`, `mix test`, `pytest`) — never from a model.

## Detach overnight

tmux:

```text
tmux new -s nightshift
nightshift run --goal "…" --repo /path/to/checkout --out ./artifacts
# detach: Ctrl-b d
```

systemd: copy [`contrib/nightshift.service`](contrib/nightshift.service), edit
the goal and paths, then `systemctl start --no-block nightshift.service`.

Progress is always appended to `artifacts/latest/run.log` (no TTY required).

## Morning checklist

1. `nightshift status` — read `PASSED` / `FAILED` / `REQUIRES_HUMAN_REVIEW`.
   `status` is the QA verdict. If Writer failed after green tests the process
   exits non-zero, `run.log` notes the missing article, and `status` still
   prints `PASSED`.
2. `git diff` in the target repo (unstaged). Edit if needed.
3. Open `artifacts/latest/05_article_draft.md` if the run used `--article`.
4. `git commit` or restore. Nightshift will not do this for you.

If QA froze (`REQUIRES_HUMAN_REVIEW`), the last apply may still be in the tree.
Restore with your usual git workflow (`git checkout -- .` / `git restore`).

## Artifacts

`artifacts/YYYY-MM-DD_<slug>/` (and `artifacts/latest`):

| File | When |
| :--- | :--- |
| `01_user_story.md` | PM |
| `02_tech_spec.md` | Tech Lead (`codegraph` / optional `graphify query`) |
| `03_diff.patch` | Dev (`git apply --check` then apply) |
| `04_qa_report.json` | QA |
| `05_article_draft.md` | Writer, only if PASSED and `--article` |
| `pipeline_state.json`, `run.log` | always |

## Development

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo llvm-cov --workspace --fail-under-lines 85
```

### Releasing

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml)
(generated by [cargo-dist](https://opensource.axo.dev/cargo-dist/); config lives
in [`dist-workspace.toml`](dist-workspace.toml)):

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`) and merge it.
2. Move the `[Unreleased]` entries in `CHANGELOG.md` into a new version section
   before tagging; `dist` uses that section as the GitHub Release notes.
3. Tag the merge commit: `git tag v0.1.0`.
4. `git push origin v0.1.0`.
5. Keep generated action pins configured in `[dist.github-action-commits]` in
   `dist-workspace.toml`; verify them with `scripts/check-action-pins.sh`.

The tag triggers `release.yml`, which first runs the CI-equivalent gate in
[`.github/workflows/release-checks.yml`](.github/workflows/release-checks.yml)
(`fmt`, `clippy`, `test`, 85% line coverage), then builds each target on its
native runner (`ubuntu-22.04`, `ubuntu-22.04-arm`, `macos-15-intel`,
`macos-14`) — no cross-compilation or OpenSSL setup, since `reqwest` uses
`rustls-tls`. It uploads the tarballs, `sha256` checksums, and the generated
`rs-nightshift-installer.sh` to a GitHub Release. Nothing is published to
crates.io.

Regenerate the workflow after editing `dist-workspace.toml` with
`dist init --yes` (or `dist generate`), and dry-run locally with
`dist build --artifacts=local --target=$(rustc -vV | sed -n 's/host: //p')`.

Pull requests are reviewed by [rs-guard](https://github.com/nebulaideas/rs-guard)
using [`.github/review-prompt.md`](.github/review-prompt.md). Set the
`DEEPSEEK_API_KEY` repository secret so the review workflow can post.

## License

MIT
