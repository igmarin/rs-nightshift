# rs-nightshift

Runs one unattended software-engineering job on a server while you sleep.

You SSH in, start `nightshift run` under tmux or systemd, and disconnect. In the
morning you review the dirty working tree and either commit or restore. The
pipeline never commits, pushes, resets, or cleans — that's your call.

## Install

```text
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/igmarin/rs-nightshift/releases/latest/download/rs-nightshift-installer.sh | sh
```

The script picks the right tarball for your platform, verifies the checksum, and
puts `nightshift` on `PATH` (`~/.cargo/bin`). Prebuilt binaries are available
for Linux (`x86_64`, `aarch64`) and macOS (`x86_64`, `aarch64`).

After installing, run `nightshift doctor` to check that your server is ready
(Ollama running, models pulled, `codegraph` and `graphify` on `PATH`). See
[docs/architecture.md](docs/architecture.md) for the full prerequisites list.

## Usage

```text
nightshift doctor                          # check server is ready
nightshift run --goal "…" --repo /path     # run the overnight pipeline
nightshift status                          # morning: read the QA verdict
```

### The overnight run

```text
nightshift run --goal "add status command" --repo ~/projects/my-app
```

This runs PM → Tech Lead → Dev → QA (up to 3 iterations) → Writer, writing
artifacts to `artifacts/YYYY-MM-DD_<slug>/`. Use `--until pm|tech-lead|dev|qa`
to stop early for debugging. Add `--allow-dirty` if the tree already has
uncommitted changes.

Detach with tmux: start a session, run inside it, then Ctrl-b d:

```text
tmux new -s nightshift
# inside tmux:
nightshift run --goal "…" --repo ~/projects/my-app
# detach: Ctrl-b d
```

Or use the bundled [`contrib/nightshift.service`](contrib/nightshift.service)
with systemd. Progress is always appended to `artifacts/latest/run.log` — no TTY
needed.

### The morning checklist

1. `nightshift status` — read `PASSED`, `FAILED`, or `REQUIRES_HUMAN_REVIEW`.
2. `git diff` in the target repo — review what changed.
3. `git commit` or `git restore` — nightshift won't do this for you.

If QA froze at `REQUIRES_HUMAN_REVIEW`, the last patch may still be in the tree.
Review `artifacts/latest/03_diff.patch` and restore selectively with
`git checkout -- <file>` or `git restore <file>`.

### Configuration

Everything works out of the box with defaults. To override the Ollama URL or
swap model tags, see [docs/configuration.md](docs/configuration.md).

## License

MIT
