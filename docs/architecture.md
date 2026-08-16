# Architecture

## Server vs laptop

```text
[laptop]  SSH start  →  [server: tmux or systemd]
                         nightshift run --goal … --repo …
                         (operator disconnects; process keeps running)
[laptop]  SSH / Zed  ←  morning: status, git diff, commit or restore
```

`nightshift` does not daemonize itself. You start it under tmux or systemd and
disconnect. The process writes progress to `run.log` — no TTY required.

## Prerequisites (server)

- Rust (mise or rustup)
- Ollama listening on `127.0.0.1:11434` with these tags:
  `llama3.2:3b`, `llama3.1:8b`, `mistral-nemo:12b`, `qwen2.5-coder:7b`,
  `deepseek-r1:7b`, `gemma2:9b`, `phi3.5:latest`
- `codegraph` and `graphify` on `PATH`
- Pre-build `.codegraph/` in the target repo (`codegraph init`). Optional:
  `graphify-out/graph.json` for Tech Lead context. Nightshift will `codegraph
  init` once if the index is missing. It never rebuilds a full graphify corpus.

Run `nightshift doctor` to verify all of the above. It exits `0` if the
environment is ready, `2` if a required check failed.

## Pipeline stages

```text
PM → Tech Lead → Dev (apply patch) → QA (run tests) → Writer (optional)
                     ↑__________________|  (max 3 iterations)
```

| Stage | Model | What it does |
| :---- | :---- | :----------- |
| PM | `llama3.1:8b` | Writes the user story from the goal |
| Tech Lead | `mistral-nemo:12b` | Writes the tech spec, using `codegraph` and optional `graphify` context |
| Dev | `qwen2.5-coder:7b` | Writes a patch and applies it with `git apply` |
| QA | `deepseek-r1:7b` | Runs the test suite, reasons about failures, writes the QA report |
| Writer | `gemma2:9b` | Writes a changelog and article draft (only if QA passed and `--article`) |
| Router | `llama3.2:3b` | Fast schema repair and payload checks (used internally by stages) |
| Aux | `phi3.5:latest` | Lightweight sanity check (used internally) |

The test command comes from `nightshift.toml` or a detector (`cargo test`,
`bundle exec rspec`, `mix test`, `pytest`) — never from a model.

## Artifacts

Each run writes to `artifacts/YYYY-MM-DD_<slug>/` (and `artifacts/latest` points
at the most recent):

| File | When |
| :--- | :--- |
| `01_user_story.md` | PM |
| `02_tech_spec.md` | Tech Lead |
| `03_diff.patch` | Dev |
| `04_qa_report.json` | QA |
| `05_article_draft.md` | Writer, only if PASSED and `--article` |
| `pipeline_state.json` | always (current stage, iteration, last error) |
| `run.log` | always (one line per stage transition) |

## Commands reference

```text
nightshift doctor [--ollama-url URL]
nightshift status [--out DIR]
nightshift run --goal TEXT --repo PATH [--ollama-url URL] [--name SLUG] [--out DIR] [--allow-dirty] [--article|--no-article] [--until pm|tech-lead|dev|qa]
```

- Omit `--until` for a full run (PM → Tech Lead → Dev → QA → Writer).
- `--until` is a debug stop — it does not skip QA in a full run.
- `--allow-dirty` is required when the target tree already has uncommitted changes.
- `--article` is on by default; use `--no-article` to skip the Writer stage.
- `--name` overrides the slug (defaults to the goal text).
- `--out` overrides the artifact root (defaults to `./artifacts`).

## Module layout

```text
src/
├── main.rs              # CLI entry point
├── cli.rs               # clap argument parsing
├── pipeline.rs          # orchestration: PM → TechLead → Dev → QA → Writer
├── pm.rs                # user-story writer
├── techlead.rs          # tech-spec writer
├── dev.rs               # patch author + git apply
├── qa.rs                # test runner + QA report
├── writer.rs            # article draft
├── generate.rs          # Generator trait (Ollama in prod, scripted in tests)
├── ollama.rs            # Ollama HTTP client + URL validation/redaction
├── context.rs           # codegraph/graphify context gathering
├── models.rs            # role-to-model config
├── doctor/
│   ├── mod.rs           # doctor command orchestration
│   ├── catalog.rs       # Ollama model catalog check
│   ├── host.rs          # host probe (PATH, mise, rustc)
│   └── report.rs        # doctor output formatting
├── artifacts/
│   ├── mod.rs           # ArtifactStore + RunDir
│   ├── state.rs         # pipeline_state.json read/write
│   ├── qa.rs            # QaReport + QaStatus
│   └── util.rs          # slugify
├── testrun.rs           # test command detection + runner
└── error.rs             # Error enum (10 typed variants)
```
