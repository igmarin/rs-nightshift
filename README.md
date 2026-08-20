# rs-nightshift

Runs one unattended software-engineering job on a server while you sleep.

You SSH in, start `nightshift run` under tmux or systemd, and disconnect. In the
morning you review the dirty working tree and either commit or restore. The
pipeline never commits, pushes, resets, or cleans — that's your call.

## How it works

```text
You:  nightshift run --goal "add status command" --repo ~/projects/my-app
        │
        ▼
  PM stage         writes 01_user_story.md      (llama3.1:8b)
        │
        ▼
  Tech Lead        writes 02_tech_spec.md       (mistral-nemo:12b)
  stage            uses codegraph + graphify for repo context
        │
        ▼
  Dev stage        writes 03_diff.patch          (qwen2.5-coder:7b)
  (max 3           applies it with git apply
   iterations)        │
        │            ▼
        │         QA stage        runs your test suite      (deepseek-r1:7b)
        │         (tests fail?)──→ reasons about the failure
        │              │               sends hints back to Dev
        │              ▼
        ▼         tests pass
  Writer stage     writes 05_article_draft.md   (gemma2:9b)
        │
        ▼
  Done.  Artifacts in artifacts/YYYY-MM-DD_<slug>/
```

The pipeline never commits, pushes, resets, or cleans. In the morning you get a
dirty working tree and a set of artifacts — you decide what to keep.

## Install

```text
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/igmarin/rs-nightshift/releases/latest/download/rs-nightshift-installer.sh | sh
```

The script picks the right tarball for your platform, verifies the checksum, and
puts `nightshift` on `PATH` (`~/.cargo/bin`). Prebuilt binaries are available
for Linux (`x86_64`, `aarch64`) and macOS (`x86_64`, `aarch64`).

No Rust toolchain required on the server — the binary is self-contained.

## Prerequisites

Before your first run, the server needs:

- **Ollama** listening on `127.0.0.1:11434` with these models pulled:
  `llama3.2:3b`, `llama3.1:8b`, `mistral-nemo:12b`, `qwen2.5-coder:7b`,
  `deepseek-r1:7b`, `gemma2:9b`, `phi3.5:latest`
- **codegraph** and **graphify** on `PATH`
- **Rust** (mise or rustup) — needed by the target repo's build, not by nightshift itself

Check everything is ready:

```text
nightshift doctor
```

Exits `0` if the server is ready, `2` if a required check failed. You can also
point at a non-default Ollama URL:

```text
nightshift doctor --ollama-url http://10.0.0.5:11434
```

## Quick start

### 1. Prepare the target repo

Nightshift needs a `.codegraph/` index in the target repo to give the Tech Lead
stage structural context. Build it once:

```text
cd ~/projects/my-app
codegraph init
```

Optional but recommended: run `graphify .` in the repo root to produce
`graphify-out/graph.json` for richer Tech Lead context.

### 2. Start the run

Use tmux so you can disconnect. **Start tmux first, then run inside it** — if
you start the run in a plain SSH session and close the terminal, the process
dies.

```text
tmux new -s nightshift
# inside tmux:
nightshift run --goal "add a /health endpoint that returns 200 OK" --repo ~/projects/my-app
# detach: Ctrl-b d
```

Before closing your terminal, verify the session is holding:

```text
tmux ls
# should show: nightshift: 1 windows
```

If you see `no server running`, the process is in the foreground and will die
when you close — go back and start tmux first.

Reattach later from any SSH session:

```text
tmux attach -t nightshift
```

Or use systemd with the bundled
[`contrib/nightshift.service`](contrib/nightshift.service) — edit the goal and
paths, then `systemctl start --no-block nightshift.service`.

Progress is always appended to `artifacts/latest/run.log` — no TTY needed.

### 3. Check progress (optional)

From another SSH session, without attaching to tmux:

```text
tail -f ~/projects/my-app/artifacts/latest/run.log
```

You'll see lines like:

```text
stage=pm
stage=pm done
stage=tech-lead
stage=tech-lead done
stage=dev iteration=1
stage=dev done
```

### 4. Morning: review the results

SSH back in and reattach (or just check the log):

```text
tmux attach -t nightshift
# or without attaching:
nightshift status --out ~/projects/my-app/artifacts
```

```text
nightshift status
```

This prints `PASSED`, `FAILED`, or `REQUIRES_HUMAN_REVIEW` based on the QA
report. Then review what changed:

```text
cd ~/projects/my-app
git diff
```

The pipeline applied a patch to your working tree (unstaged). Review it, edit if
needed, and either commit or restore:

```text
git commit -am "add /health endpoint"
# or restore:
git restore <file>    # selective
```

Nightshift won't do this for you.

## Artifacts

Each run writes to `artifacts/YYYY-MM-DD_<slug>/` (and `artifacts/latest` points
at the most recent):

| File | When |
| :--- | :--- |
| `01_user_story.md` | PM stage |
| `02_tech_spec.md` | Tech Lead stage |
| `03_diff.patch` | Dev stage (also applied to the working tree) |
| `04_qa_report.json` | QA stage |
| `05_article_draft.md` | Writer stage (only if QA passed and `--article`) |
| `pipeline_state.json` | Always (current stage, iteration, last error) |
| `run.log` | Always (one line per stage transition) |

## Commands

```text
nightshift doctor [--ollama-url URL] [--config PATH]
nightshift status [--out DIR]
nightshift run --goal TEXT --repo PATH [options]
nightshift harness --goal TEXT --repo PATH [--config PATH] [--out DIR] [--name SLUG]
nightshift plan --goal TEXT --repo PATH [--config PATH] [--out DIR] [--name SLUG]
```

`harness` is the new config-driven role-graph engine (beta): it reads a
role-graph `nightshift.toml`, walks the roles, routing on each role's verdict,
and writes a morning report. `plan` is the pre-flight companion: it runs the
entry role and, when it raises clarifying questions, asks you for answers
interactively until the brief is clear. `--repo` is required on both (it is
where capabilities like `apply-patch` operate). See
[`docs/role-graph.md`](docs/role-graph.md) and
[`nightshift.toml.example`](nightshift.toml.example).

### `run` options

| Flag | Default | What it does |
| :--- | :------ | :----------- |
| `--goal` | (required) | The business goal in plain English |
| `--repo` | (required) | Path to the target git checkout |
| `--name` | derived from goal | Override the artifact slug |
| `--out` | `./artifacts` | Override the artifact root |
| `--ollama-url` | `http://127.0.0.1:11434` | Ollama HTTP origin |
| `--until` | (full run) | Stop after `pm`, `tech-lead`, `dev`, or `qa` |
| `--article` / `--no-article` | `--article` | Enable/disable the Writer stage |
| `--allow-dirty` | off | Allow running on a tree with uncommitted changes |

`--until` is a debug stop — it halts the pipeline early so you can inspect
intermediate artifacts. The test command is detected automatically (`cargo test`,
`bundle exec rspec`, `mix test`, `pytest`) or set in `nightshift.toml` — never
guessed by a model.

## Configuration

Everything works out of the box with defaults. Two things you might override:

**Ollama URL** — set via CLI flag or env var (not in `nightshift.toml`):

```text
export NIGHTSHIFT_OLLAMA_URL=http://10.0.0.5:11434
```

**Model tags** — override per role in `nightshift.toml`:

```toml
[role_models]
Dev = "qwen2.5-coder:14b"
Qa = "deepseek-r1:14b"
```

A sample file is at [`nightshift.toml.example`](nightshift.toml.example). Copy
it to `nightshift.toml` in the directory where you run `nightshift`.

Precedence: **CLI flag > env var > `nightshift.toml` > built-in default**.

For the full config reference, see [docs/configuration.md](docs/configuration.md).

## What to do when QA freezes

If `nightshift status` prints `REQUIRES_HUMAN_REVIEW`, the pipeline tried 3 Dev
iterations and tests still failed. The last patch may still be applied to your
working tree.

1. Read `artifacts/latest/04_qa_report.json` for the failure summary.
2. Read `artifacts/latest/03_diff.patch` to see what was applied.
3. Review `git diff` in the target repo.
4. Fix it yourself, or restore selectively with `git restore <file>`.

## Code layout

The crate is layered hexagonally (ADR-007): a pure domain, ports, an
application layer, and adapters — domain and application never do I/O. Each
folder has a README describing its responsibility and ownership:

- [src/README.md](src/README.md) — crate layout and the hexagonal layers
- [src/domain/README.md](src/domain/README.md) — pure domain: role-graph config, verdicts, routing, state
- [src/application/README.md](src/application/README.md) — use cases: executor, orchestrator, report
- [src/adapters/README.md](src/adapters/README.md) — the only I/O layer: providers, capabilities, stores, clock
- [src/adapters/providers/README.md](src/adapters/providers/README.md) — the ModelClient provider adapters (Ollama / OpenAI-compatible) + factory

## Further reading

- [docs/architecture.md](docs/architecture.md) — the role-graph harness: hexagon, commands, providers, capabilities, state/report
- [docs/role-graph.md](docs/role-graph.md) — role-graph design and the full config schema
- [docs/configuration.md](docs/configuration.md) — full config reference with examples
- [docs/decisions.md](docs/decisions.md) — why things are the way they are (ADRs)
- [docs/development.md](docs/development.md) — contributing, CI gates, PR workflow
- [docs/release.md](docs/release.md) — how to cut a release

## License

MIT
