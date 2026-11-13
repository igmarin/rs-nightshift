# rs-nightshift

Runs coding jobs on local models while you sleep, and leaves the changes in
the working tree for you to review in the morning.

You give nightshift a goal, a repo, and a config of roles, for example a
product owner, a developer and QA. Each role is one model call. Nightshift
walks the roles, applies the edits the developer writes, runs the repo's
tests for QA, and stops when QA is satisfied or a limit is reached. It never
commits, pushes, resets or cleans: you read the report and `git diff`, then
commit or restore.

Each role can use its own provider and model: Ollama, any OpenAI-compatible
server (rapid-mlx, llama.cpp, LM Studio), or a hosted API such as DeepSeek.
One config can map the roles to different models on different machines.

## How a run works

```text
nightshift harness --goal "add a --json flag to status" --repo ~/src/app

  product-owner   writes 01_brief.md
        │ continue
        ▼
  developer       gets the brief and the repo files; replies with edits,
        │         which nightshift applies to the working tree
        │ continue
        ▼
  qa              nightshift runs the repo's tests first; qa reads the result
        │ issues ──► back to developer with the findings (at most max_loop times)
        │ done
        ▼
  report          status, steps, retries, time per role
```

Each role ends its reply with a verdict (`continue`, `issues`, `questions`,
`done` or `fail`), and the config maps each verdict to the next role. No model
picks the next step. The roles, prompts, models and routes all live in
`nightshift.toml`; [`docs/role-graph.md`](docs/role-graph.md) explains the
design.

## Install

```text
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/igmarin/rs-nightshift/releases/latest/download/rs-nightshift-installer.sh | sh
```

The script downloads the binary for your platform (Linux or macOS, `x86_64`
or `aarch64`), checks its checksum, and puts `nightshift` in `~/.cargo/bin`.
Nightshift itself needs no Rust toolchain. From a checkout, `cargo install
--path .` builds it.

## Quick start

1. Start a model server and get the models. With Ollama, for the example's
   `linux` profile: `ollama pull llama3.1:8b` and
   `ollama pull qwen2.5-coder:7b`. On a Mac with rapid-mlx, see
   [`docs/macos.md`](docs/macos.md).
2. Copy the example config and set your models:

   ```text
   cp nightshift.toml.example nightshift.toml
   ```

   It defines a `mac` profile (rapid-mlx) and a `linux` profile (Ollama).
   Pick one with `--profile linux` or `NIGHTSHIFT_PROFILE=linux`.
3. Check the setup:

   ```text
   nightshift doctor
   ```

   It reads `nightshift.toml` (or `--config FILE`) and checks the model
   servers and models the roles use, and `codegraph` and `graphify` when a
   role uses `gather-context`. It exits 0 when everything required passes,
   2 otherwise.
4. Optional: check that a model handles the reply format before a long run:

   ```text
   nightshift bench --model @coder
   ```

5. Start the run in tmux, so it keeps running after you disconnect:

   ```text
   tmux new -s nightshift
   nightshift harness --goal "add a --json flag to the status command" --repo ~/src/app
   # detach with Ctrl-b d; reattach later with: tmux attach -t nightshift
   ```

6. In the morning, read the report that `harness` printed, then the change:

   ```text
   cd ~/src/app
   git diff
   git commit -am "add --json to status"   # or: git restore <file>
   ```

`harness` exits 0 when QA said done (`PASSED`), 1 when a role failed
(`FAILED`), and 2 when the run stopped for a person to look
(`REQUIRES_HUMAN_REVIEW`: a blocking question, or a run that hit `max_loop`
or `max_steps`). It does not check whether the repo has uncommitted changes, so
start from a clean tree to see only the run's edits.

## The run directory

Each run writes to `artifacts/{date}_{slug}-run-{timestamp}/` (`--out`
changes `artifacts`, `--name` changes the slug):

| File | What it holds |
| :--- | :--- |
| `01_brief.md`, `02_change.txt`, … | each role's `output` file from the config |
| `actions.jsonl` | one line per event: role start and end, each model call with its time and tokens, retries, loops, the end of the run |
| `state.json` | the last status: current role, steps, loop counts, block reason |
| `raw_response_<role>_<attempt>.txt` | each reply that was rejected and retried |

## Overnight: a queue of goals

`nightshift queue` runs many goals back to back, one at a time:

```text
nightshift queue --file goals.toml --config nightshift.toml --out ~/nightshift/night-1
```

A failed job does not stop the queue, and running the same command again
skips the jobs already done. Nightshift never commits, so give each job that
edits code its own clean checkout: a job whose repo has uncommitted changes
is skipped as `blocked: dirty_tree`. Progress is in `queue_report.md` under
`--out`. See [Goal queue](docs/configuration.md#goal-queue-nightshift-queue).

To keep a queue running with no terminal open:

- Linux: the systemd unit in
  [`contrib/nightshift.service`](contrib/nightshift.service) runs a queue.
- macOS: the launchd agents in [`contrib/`](contrib/) wait for the model
  server and keep the Mac awake. See [`docs/macos.md`](docs/macos.md).

## Commands

| Command | What it does |
| :--- | :--- |
| `nightshift harness --goal TEXT --repo PATH` | one run of the role graph |
| `nightshift plan --goal TEXT --repo PATH` | runs only the first role, asks you its questions on the terminal (up to 5 rounds), and writes the brief |
| `nightshift queue --file FILE [--stop-on-failure]` | many runs back to back |
| `nightshift bench --model TAG [--provider NAME] [--runs N] [--timeout S]` | three short tasks that show whether a model can follow the reply format |
| `nightshift doctor` | checks the config, model servers, models and tools |
| `nightshift run`, `nightshift status` | the older fixed pipeline: see [`docs/legacy-run.md`](docs/legacy-run.md) |

`harness`, `plan`, `queue`, `doctor` and `bench` take `--config FILE`
(default `nightshift.toml`); `harness`, `plan` and `queue` take `--out DIR`
(default `./artifacts`). Every command takes `--profile NAME` (or
`NIGHTSHIFT_PROFILE`) to pick the machine profile.

`doctor` checks Ollama at `--ollama-url` (default `http://127.0.0.1:11434`),
not at `[providers.ollama] base_url`. If you moved Ollama, pass both.

## What nightshift does and does not do

- It never commits, pushes, resets or cleans.
- Models only write text. Nightshift makes every change itself, through the
  tools a role declares (`search-replace`, `write-file`, `apply-patch`), and
  only inside the repo. They refuse paths such as `.git/`, `.env` and key
  files.
- The test command comes from the repo (`cargo test`, `bundle exec rspec`,
  `mix test`, `pytest`, or a `[test]` command in the repo's
  `nightshift.toml`), never from a model.
- A rejected reply is retried with the reason (`retries`, default 2); a role
  that loops too often stops the run (`max_loop`, `max_steps`).

## Documentation

- [docs/configuration.md](docs/configuration.md): every config key, profiles,
  tools, retries, `bench` and `queue`
- [docs/macos.md](docs/macos.md): unattended runs on a Mac
- [docs/ollama.md](docs/ollama.md): Ollama on CPU-only and integrated-GPU machines
- [docs/role-graph.md](docs/role-graph.md): the design of the role graph
- [docs/architecture.md](docs/architecture.md): how the code is organized
- [docs/decisions.md](docs/decisions.md): why things are the way they are
- [docs/development.md](docs/development.md): checks, CI and the PR workflow
- [docs/release.md](docs/release.md): how to cut a release
- [docs/legacy-run.md](docs/legacy-run.md): the older `nightshift run` pipeline

## License

MIT
