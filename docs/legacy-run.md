# The legacy pipeline: `nightshift run`

`nightshift run` and `nightshift status` are the first version of nightshift:
a fixed pipeline of five stages on Ollama. The role graph (`nightshift
harness`) replaces it ([ADR-006](decisions.md)). Both commands still work
until they are removed; for a new setup, use `harness` (see the
[README](../README.md)).

## The stages

| Stage | Model (default) | Writes |
| :--- | :--- | :--- |
| PM | `llama3.1:8b` | `01_user_story.md` |
| Tech Lead | `mistral-nemo:12b` | `02_tech_spec.md`, with codegraph and graphify context |
| Dev | `qwen2.5-coder:7b` | `03_diff.patch`, applied to the working tree with `git apply` |
| QA | `deepseek-r1:7b` | `04_qa_report.json`, after running the repo's tests; on a failure it sends hints back to Dev, at most 3 times |
| Writer | `gemma2:9b` | `05_article_draft.md`, only when QA passed and `--article` is on |

Two more model slots exist: Router (`llama3.2:3b`) and Aux (`phi3.5:latest`).
Pull the models before a run: `nightshift doctor` checks the role-graph
config, not these models.

## Running it

```text
nightshift run --goal "add a /health endpoint that returns 200 OK" --repo ~/projects/my-app
```

| Flag | Default | What it does |
| :--- | :--- | :--- |
| `--goal` | (required) | the goal in plain English |
| `--repo` | (required) | the target git checkout |
| `--name` | from the goal | the run directory's slug |
| `--out` | `./artifacts` | the artifact root |
| `--ollama-url` | `http://127.0.0.1:11434` | the Ollama address (or `NIGHTSHIFT_OLLAMA_URL`) |
| `--until` | (full run) | stop after `pm`, `tech-lead`, `dev` or `qa`, to look at the files so far |
| `--article` / `--no-article` | `--article` | turn the Writer stage on or off |
| `--allow-dirty` | off | run on a repo with uncommitted changes |

The Ollama URL must be `http` or `https` with a host, and no path, query,
fragment or user credentials; `doctor` and `run` reject anything else.

## Files

Each run writes to `artifacts/YYYY-MM-DD_<slug>/`, and `artifacts/latest`
points at the most recent run:

| File | When |
| :--- | :--- |
| `01_user_story.md` | PM stage |
| `02_tech_spec.md` | Tech Lead stage |
| `03_diff.patch` | Dev stage (also applied to the working tree, unstaged) |
| `04_qa_report.json` | QA stage |
| `05_article_draft.md` | Writer stage |
| `pipeline_state.json` | always: current stage, iteration, last error |
| `run.log` | always: one line per stage, such as `stage=dev iteration=1` |

`nightshift status [--out DIR]` prints `PASSED`, `FAILED` or
`REQUIRES_HUMAN_REVIEW` from the latest QA report.

## Models: `[role_models]`

Override a stage's model in `nightshift.toml` (or the file named by
`NIGHTSHIFT_CONFIG`):

```toml
[role_models]
Dev = "qwen2.5-coder:14b"
Qa = "deepseek-r1:14b"
```

The names are `Router`, `Pm`, `TechLead`, `Dev`, `Qa`, `Writer` and `Aux`;
other names are ignored. Precedence: command-line flag, then environment
variable, then `nightshift.toml`, then the built-in default.

`harness`, `plan`, `queue` and `doctor` ignore `[role_models]`. They need a
role graph (`[run]` and `[[roles]]`) in the same file, so a file with only
`[role_models]` makes `doctor` fail.

## When QA gives up

`REQUIRES_HUMAN_REVIEW` means Dev tried 3 times and the tests still failed.
The last patch may still be applied:

1. Read `artifacts/latest/04_qa_report.json` for the failure.
2. Read `artifacts/latest/03_diff.patch` for what was applied.
3. Check `git diff` in the repo, then fix it or `git restore <file>`.
