# Unattended runs on macOS

How to keep a [goal queue](configuration.md#goal-queue-nightshift-queue)
running on a Mac with the terminal closed and the machine awake. This is the
macOS counterpart of the systemd unit in
[`contrib/nightshift.service`](../contrib/nightshift.service).

Three things differ from a Linux server:

| Problem | Answer |
| :--- | :--- |
| No systemd | A launchd user agent: [`contrib/dev.nightshift.queue.plist`](../contrib/dev.nightshift.queue.plist) |
| The Mac sleeps when idle | The agent runs the queue under `caffeinate -i` |
| The model server must be up first | Start it yourself, or load [`contrib/dev.nightshift.model-server.plist`](../contrib/dev.nightshift.model-server.plist) |

## 1. Lay out the files

The sample agent expects this layout. Any layout works if you edit the paths.

```text
~/nightshift/
  goals.toml        # the queue file
  nightshift.toml   # the role-graph config
  artifacts/        # run directories, queue_report.md, queue.log
```

```text
mkdir -p ~/nightshift/artifacts
```

The `artifacts` directory must exist before the agent is loaded, because
launchd opens the log file there. Keep it outside the repos the jobs edit
(see [Goal queue](configuration.md#goal-queue-nightshift-queue)).

## 2. Start the model server

Nightshift talks to any OpenAI-compatible server
([config](configuration.md#local-openai-compatible-servers-rapid-mlx-llamacpp-lm-studio)).
With rapid-mlx:

```text
rapid-mlx serve qwen3.6-35b-4bit --port 8000 --served-model-name qwen3.6-35b-4bit
```

To have launchd start it at login and start it again if it exits, install the
sample agent. Edit `MODEL` (twice) and the `rapid-mlx` path (`which rapid-mlx`)
in the copy:

```text
sed "s|/Users/YOU|$HOME|g" contrib/dev.nightshift.model-server.plist \
  > ~/Library/LaunchAgents/dev.nightshift.model-server.plist
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/dev.nightshift.model-server.plist
curl -s http://127.0.0.1:8000/v1/models
```

Its log is `~/Library/Logs/nightshift-model-server.log`. One agent runs one
server. For a second server, copy the file with another `Label` and port, and
add a second `[providers.*]` entry to the config.

## 3. Check the environment launchd will use

launchd does not read your shell profile. A job starts with
`PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so `cargo`, `codegraph` and `graphify`
are not found unless the agent's `PATH` lists their directories. Check with
the same `PATH` you will put in the agent:

```text
env -i HOME="$HOME" \
  PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  ~/.cargo/bin/nightshift doctor --config ~/nightshift/nightshift.toml
```

Every required check must be `[ok]`. If a tool is `not found on PATH`, add
its directory (`dirname "$(which TOOL)"`) to `PATH` here and in the agent.

## 4. Install and start the queue agent

```text
sed "s|/Users/YOU|$HOME|g" contrib/dev.nightshift.queue.plist \
  > ~/Library/LaunchAgents/dev.nightshift.queue.plist
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/dev.nightshift.queue.plist
```

`bootstrap` loads the agent and starts the queue at once. You can close the
terminal. To pick a machine profile, add `--profile NAME` to the arguments in
the file, or set `profile` per job in the queue file.

| Task | Command |
| :--- | :--- |
| Is it running? | `launchctl print gui/$UID/dev.nightshift.queue \| grep -E "state =\|pid =\|last exit code"` |
| Follow the log | `tail -f ~/nightshift/artifacts/queue.log` |
| See the results | `cat ~/nightshift/artifacts/queue_report.md` |
| Stop it | `launchctl bootout gui/$UID/dev.nightshift.queue` |
| Run the queue again | `launchctl kickstart gui/$UID/dev.nightshift.queue` |
| Remove it | `bootout` as above, then delete the file in `~/Library/LaunchAgents` |

`last exit code = 0` means every job is done; `1` means at least one job is
blocked or failed (the report says which).

The agent has no `KeepAlive`, so a finished queue is not started again. It
does start at each login while the file is in `~/Library/LaunchAgents`. That
resumes a queue after a reboot: jobs already done are skipped. Delete the
file when you no longer want that.

After you change the file, `bootout` and `bootstrap` again. launchd does not
reread it.

## 5. Sleep and power

- `caffeinate -i` stops the Mac from sleeping when idle for as long as the
  queue runs, then lets it sleep again. Confirm with
  `pmset -g assertions | grep caffeinate`.
- The display may sleep. That does not affect the run.
- `caffeinate` does not stop a sleep you ask for (Apple menu, power button) or
  a closed laptop lid. Keep a MacBook open and on power.
- Low Power Mode can slow generation. Check with `pmset -g | grep lowpowermode`
  (`1` is on). Turn it off with `sudo pmset -a lowpowermode 0`, or in System
  Settings under Energy.
- To start the Mac again after a power cut: `sudo pmset -a autorestart 1`.

## 6. Without a display

A Mac runs the queue the same way with no monitor attached. Set these up
while the monitor is still connected:

1. Turn on Remote Login (System Settings, General, Sharing), so you can reach
   the Mac with `ssh`. Screen Sharing is optional.
2. Log in on the Mac and leave that user logged in. A locked screen is fine.
   The queue agent is a user agent: launchd runs it only while its user has a
   login session on the Mac itself.
3. Load the agent (step 4), then disconnect the monitor.

After a restart, nobody is logged in, so the agent does not start until that
user logs in again. Automatic login covers this, but macOS does not offer it
while FileVault is on.

If you only have an `ssh` session and nobody is logged in on the Mac, there
is no `gui/$UID` domain to load the agent into. Start the queue with `nohup`
instead, so it survives the end of the `ssh` session. An `ssh` shell has your
normal `PATH`:

```text
nohup caffeinate -i nightshift queue --file ~/nightshift/goals.toml \
  --config ~/nightshift/nightshift.toml --out ~/nightshift/artifacts \
  >> ~/nightshift/artifacts/queue.log 2>&1 &
```

## 7. Memory

Apple Silicon shares one memory pool between the system and the models, so
every model a config uses must fit in RAM at the same time, with room for
context.

- A 4-bit model needs roughly half a gigabyte per billion parameters, plus
  its context. Measured on an M2 Ultra with 128 GB: three 4-bit models (27B,
  31B and 35B) loaded together used about 44 GB, with no swap.
- Check while a run is active: `sysctl vm.swapusage` should show `used = 0`.
  If it grows, use fewer or smaller models, or one model for every role.
- Speed varies more than size: those three models ran at 8 to 49 tokens per
  second on the same machine. Try a model on a real goal, and with
  [`nightshift bench`](configuration.md#benchmark-a-model-nightshift-bench),
  before giving it a role.
