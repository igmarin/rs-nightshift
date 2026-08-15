# rs-nightshift

Rust CLI that runs **one** unattended software-engineering job on a **server**.
The operator SSHs in, starts `nightshift` under tmux or systemd, and disconnects.
In the morning they review the dirty working tree in Zed (or a terminal) and
either commit or restore.

The pipeline never commits, pushes, resets, or cleans.

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
nightshift doctor
```

`doctor` exits `0` if the environment is ready, `2` if a required check failed.

## Commands

```text
nightshift doctor
nightshift status [--out DIR]
nightshift run --goal TEXT --repo PATH [--name SLUG] [--out DIR] [--allow-dirty] [--article|--no-article] [--until pm|tech-lead|dev|qa]
```

Omit `--until` for a full run: PM → Tech Lead → Dev apply → QA (max 3) → Writer
(if `--article`, default on, and QA `PASSED`).

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

Pull requests are reviewed by [rs-guard](https://github.com/nebulaideas/rs-guard)
using [`.github/review-prompt.md`](.github/review-prompt.md). Set the
`DEEPSEEK_API_KEY` repository secret so the review workflow can post.

## License

MIT
