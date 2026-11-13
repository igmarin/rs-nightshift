# Ollama tuning

How to make Ollama fast enough for nightshift on a machine without a
discrete GPU. The numbers on this page were measured in August 2026 on an
AMD Ryzen 7 7730U laptop (8 cores, integrated Radeon graphics).

## How nightshift calls Ollama

- One completion at a time.
- After each completion, nightshift asks Ollama to unload the model
  (`keep_alive: 0`), so every role call loads it again. `OLLAMA_KEEP_ALIVE`
  does not change this: it only affects other clients, such as `ollama run`.
- Each call has a time limit: 3600 s by default, or the role's
  `timeout_secs` (see [configuration](configuration.md)).
- A role's `options` can set `temperature`, `max_tokens` and `think`.
  `num_ctx` is not sent to Ollama (`nightshift doctor` warns about it): set
  the context size in a Modelfile, as below.

## 1. Ollama service settings

`/etc/systemd/system/ollama.service.d/override.conf`:

```ini
[Service]
Environment="OLLAMA_NUM_THREAD=8"
Environment="OLLAMA_VULKAN=1"
Environment="OLLAMA_IGPU_ENABLE=1"
```

```bash
sudo systemctl daemon-reload
sudo systemctl restart ollama
```

| Variable | Why |
| :--------- | :-- |
| `OLLAMA_NUM_THREAD=8` | The 7730U has 8 physical cores and 16 threads. All 16 threads were slightly slower than 8 for this workload. Use your physical core count. |
| `OLLAMA_VULKAN=1` | Lets Ollama look for a Vulkan GPU. |
| `OLLAMA_IGPU_ENABLE=1` | Lets Ollama consider integrated GPUs. On most shared-memory iGPUs it still runs `100% CPU` (see [below](#why-the-igpu-is-not-used)), but you need it to test GPU offload. |

## 2. A tuned model

Put the context size and thread count in a model variant instead of global
settings:

```bash
cat > /tmp/Modelfile.llama3.2.3b.fast <<'EOF'
FROM llama3.2:3b
PARAMETER num_ctx 2048
PARAMETER num_thread 8
PARAMETER num_gpu 0
PARAMETER temperature 0.2
EOF

ollama create llama3.2:3b-fast -f /tmp/Modelfile.llama3.2.3b.fast
```

| Parameter | Why |
| :-------- | :-- |
| `num_ctx 2048` | Enough for the short prompts of the [demo](#running-the-demo), and half the 4096 default, so the KV cache uses less RAM. Real tasks with source files need more; see [Which model](#which-model). |
| `num_thread 8` | Overrides `OLLAMA_NUM_THREAD` for this model. |
| `num_gpu 0` | CPU only. Vulkan cannot offload to this shared-memory iGPU, so this avoids trying. |
| `temperature 0.2` | Low enough for repeatable JSON and edits. A role's `options.temperature` overrides it. |

For another model, change the `FROM` line and the tag:

```bash
cat > /tmp/Modelfile.MODEL.fast <<'EOF'
FROM <model:tag>
PARAMETER num_ctx 2048
PARAMETER num_thread 8
PARAMETER num_gpu 0
PARAMETER temperature 0.2
EOF

ollama create <model:tag>-fast -f /tmp/Modelfile.MODEL.fast
```

With a discrete GPU that has its own VRAM, remove `num_gpu 0` or raise it so
Ollama offloads as many layers as fit. With an integrated GPU that shares
system RAM, `num_gpu 0` is usually fastest.

## 3. Use it in `nightshift.toml`

```toml
[[roles]]
id = "product-owner"
provider = "ollama"
model = "llama3.2:3b-fast"
options = { temperature = 0.3, max_tokens = 2048 }
# ...
```

## Which model

From a real three-role run (Product Owner, Developer, QA) on the 7730U: a
rewrite of the copy on a personal website.

| Model | Speed | Result |
| :---- | :---- | :----- |
| `llama3.2:3b` | about 3 tokens/s | Wrote placeholders such as `change from '...' to '...'` instead of quoting the file. |
| `qwen3:4b` | about 2 tokens/s | Spent the whole `max_tokens` (3072) on reasoning and never wrote its reply. |
| `llama3.1:8b` | about 2 tokens/s | Valid replies and a usable brief. Developer and QA finished on small files. |
| `gemma3:12b` | about 0.5 to 1 token/s | Not used: too slow for the run. |

With `llama3.1:8b` and a 32k context, reading the prompt took 15 to 20
minutes and writing the reply 20 to 40 minutes per role. The settings that
worked:

```text
FROM llama3.1:8b
PARAMETER num_ctx 32768
PARAMETER num_thread 8
PARAMETER num_gpu 0
PARAMETER temperature 0.2
```

| Role | `temperature` | `max_tokens` |
| :--- | :------------ | :----------- |
| Product Owner | 0.3 | 2048 to 3072 |
| Developer | 0.2 | 4096 to 8192 |
| QA | 0.2 | 1024 to 2048 |

What the run showed:

- `write-file` makes the model write the whole file again. At 2 tokens/s a
  file over about 10 KB did not finish in 60 minutes. `apply-patch` failed
  too: the model got the diff's context lines wrong. Use `search-replace`,
  which sends only the lines that change.
- Give each job one file when you can. A shorter prompt is read faster.

## Benchmark summary

Measured with `llama3.2:3b` and `llama3.2:3b-fast`:

| Configuration | Warm eval rate | Harness, average of 3 runs |
| :------------ | :------------- | :------------------------- |
| `q8_0` KV cache + flash attention | about 11 tokens/s | over 1 minute (one run timed out at 6 minutes) |
| 16 threads + keep_alive | about 16 tokens/s | over 1 minute, with outliers |
| 8 threads + keep_alive | about 17.4 tokens/s | about 49 s |
| 8 threads + `llama3.2:3b-fast` | about 17.4 tokens/s | 43 to 60 s |

`keep_alive` in this table is the Ollama setting during the measurement.
Nightshift itself unloads the model after each call, and loading
`llama3.2:3b` took about 1.3 s.

The `q8_0` KV cache and flash attention are not worth it on a CPU: they add
work when there is no GPU.

## Thinking models

Models such as `qwen3` and `deepseek-r1` write a `<think>…</think>` block
before the JSON reply the harness expects. The parser removes only the think
block that contains the first `{` (a draft of the JSON in the reasoning).
Literal `<think>` text inside a JSON `content` string is left alone, also
when a block before it was removed.

`/no_think` does not work reliably through Ollama's OpenAI-compatible API,
so the harness does not use it. A thinking model can still spend all of
`max_tokens` on reasoning and never write the reply, as `qwen3:4b` did above.
On a CPU-only machine, use a non-thinking model.

## Why the iGPU is not used

`ollama ps` will likely still show `100% CPU` with `OLLAMA_VULKAN=1` and
`OLLAMA_IGPU_ENABLE=1`. Ollama's Vulkan backend skips integrated GPUs that
share system RAM, because it cannot reserve the block of VRAM it expects.
GPU offload needs a discrete GPU with its own VRAM.

## Probe a model before a full run

On a CPU, a failed harness run costs 30 to 60 minutes. `nightshift bench
--model <tag>` takes minutes: see
[Benchmark a model](configuration.md#benchmark-a-model-nightshift-bench).

## Running the demo

[`harness-demo.toml`](harness-demo.toml) is a small three-role config that
improves the copy of a one-page site. Its roles use `gather-context`, so
`codegraph` must be on `PATH`: the first role runs `codegraph init` in the
test repo. Create the `llama3.2:3b-fast` model (step 2) first.

```bash
NIGHTSHIFT_SRC=~/src/rs-nightshift   # your checkout of this repo

# a test repo
rm -rf /tmp/test-repo
mkdir /tmp/test-repo
cd /tmp/test-repo
git init -b main
cat > index.html <<'EOF'
<!DOCTYPE html>
<html>
<head><title>My Site</title></head>
<body>
<h1>Welcome to my site</h1>
<p>This is a basic homepage.</p>
<a href="/about">About</a>
</body>
</html>
EOF
git add index.html
git commit -m "initial"

# the run
cp "$NIGHTSHIFT_SRC/docs/harness-demo.toml" /tmp/test-nightshift.toml
nightshift harness \
  --config /tmp/test-nightshift.toml \
  --goal "Improve homepage copy" \
  --repo /tmp/test-repo \
  --out /tmp/test-artifacts
```

The report starts with:

```text
Status: PASSED
Steps: 3
Roles: product-owner → developer → qa
```

## Troubleshooting

- "model did not return a JSON object": check that the role's prompt asks
  for a JSON object and that the model is not a thinking model that ran out
  of `max_tokens` (see [Thinking models](#thinking-models)).
- The demo takes 3 to 6 minutes: the model is writing long extra sections.
  This is usually the prompt, not Ollama. Ask for exactly two copy changes
  and say that nothing else may be added.
- A role ends with a timeout: the model is too slow for the prompt, or
  `max_tokens` is too high for the time limit. Use a smaller or tuned model,
  or raise the role's `timeout_secs` (default 3600 s).
