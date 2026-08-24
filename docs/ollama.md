# Ollama Tuning

rs-nightshift ships with built-in defaults that work on a typical local Ollama
setup, but on CPU-only machines (especially laptops with integrated GPUs) a few
Ollama tweaks make the difference between unusable slowness and a practical
pipeline.

This guide covers the recommended Ollama service configuration, how to create
tuned model variants with `Modelfile`s, and the benchmarked settings for the
`harness` demo.

## Quick start: the best CPU-only setup we found

These settings were validated on an AMD Ryzen 7 7730U with integrated Radeon
Graphics (no discrete GPU):

### 1. Ollama systemd override

`/etc/systemd/system/ollama.service.d/override.conf`:

```ini
[Service]
Environment="OLLAMA_NUM_THREAD=8"
Environment="OLLAMA_VULKAN=1"
Environment="OLLAMA_IGPU_ENABLE=1"
Environment="OLLAMA_KEEP_ALIVE=24h"
```

```bash
sudo systemctl daemon-reload
sudo systemctl restart ollama
```

| Variable | Why |
| :--------- | :-- |
| `OLLAMA_NUM_THREAD=8` | The 7730U has 8 physical cores / 16 threads. Using all 16 SMT threads is slightly slower for this workload, so 8 is the sweet spot. Adjust to your physical core count. |
| `OLLAMA_VULKAN=1` | Enable Vulkan backend discovery. |
| `OLLAMA_IGPU_ENABLE=1` | Allow Ollama to consider integrated GPUs. On most shared-memory iGPUs this still results in `100% CPU`, but it is required to test GPU offload. |
| `OLLAMA_KEEP_ALIVE=24h` | Keep models loaded for 24 hours. Prevents ~1.3s cold-start reloads between harness runs and keeps the CPU/GPU context warm. |

### 2. A tuned `llama3.2:3b-fast` model

Create a tuned variant instead of relying on global env vars for context size
and thread count:

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
| `num_ctx 2048` | Enough for the short prompts in the harness demo; smaller than the 4096 default, so KV cache uses less RAM. |
| `num_thread 8` | Per-model override of the global `OLLAMA_NUM_THREAD`. Locks the tuned model to the best physical-core count. |
| `num_gpu 0` | Force CPU-only inference. On this laptop Vulkan cannot offload to the shared-memory iGPU, so forcing CPU avoids any GPU-fallback overhead. |
| `temperature 0.2` | Deterministic enough for reproducible JSON/patch output without being too rigid. |

### 3. Use it in `nightshift.toml`

```toml
[[roles]]
id = "product-owner"
provider = "ollama"
model = "llama3.2:3b-fast"
options = { temperature = 0.3, max_tokens = 2048 }
# ...
```

## Template for other models

You can apply the same pattern to any Ollama model by replacing the `FROM`
line and the resulting tag:

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

Then set `model = "<model:tag>-fast"` in `nightshift.toml`.

If you have a discrete GPU with dedicated VRAM, remove or raise `num_gpu 0` so
Ollama offloads as many layers as possible. For integrated GPUs that share
system RAM, `num_gpu 0` is usually the fastest option.

## Benchmark summary

Measured on the Ryzen 7 7730U with `llama3.2:3b` / `llama3.2:3b-fast`:

| Configuration | Warm eval rate | Harness 3-run average |
| :------------ | :------------- | :-------------------- |
| `q8_0` KV cache + flash attention | ~11 tok/s | 1m+ (one run timed out at 6m) |
| 16 threads + keep_alive | ~16 tok/s | ~1m+ with outliers |
| **8 threads + keep_alive** | **~17.4 tok/s** | **~49s** |
| **8 threads + `llama3.2:3b-fast`** | **~17.4 tok/s** | **~43–60s** |

The `q8_0` KV cache and flash-attention settings are not recommended for
CPU-only inference. They add overhead when no GPU is available.

## Thinking models

Models such as `qwen3` and `deepseek-r1` emit a `<think>…</think>` reasoning
preamble before the JSON envelope the harness expects. The parser strips those
blocks only when the first `{` sits inside a think span, so literal
`<think>` text inside a JSON `content` string is left alone.

`/no_think` is unreliable through Ollama's OpenAI-compatible API, so the
harness does not depend on it. Thinking models still spend tokens on reasoning
and can hit `max_tokens` without ever writing JSON — on CPU-only machines
prefer a non-thinking model such as `llama3.2:3b-fast`.

## Why the iGPU is not used

`ollama ps` will likely still report `100% CPU` even with
`OLLAMA_VULKAN=1` and `OLLAMA_IGPU_ENABLE=1`. Ollama's Vulkan backend skips
integrated GPUs that share system RAM because it cannot reserve the contiguous
VRAM block it expects. A discrete AMD GPU with dedicated VRAM is required for
GPU offload.

## Running the demo

The repo includes a minimal harness demo config at
[`harness-demo.toml`](harness-demo.toml). Use it with the test repo below:

```bash
# create test repo
cd /tmp
rm -rf test-repo
mkdir test-repo
cd test-repo
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

# create tuned model
ollama create llama3.2:3b-fast -f /tmp/Modelfile.llama3.2.3b.fast

# run harness
cp docs/harness-demo.toml /tmp/test-nightshift.toml
nightshift harness \
  --config /tmp/test-nightshift.toml \
  --goal "Improve homepage copy" \
  --repo /tmp/test-repo \
  --out /tmp/test-artifacts
```

Expected output:

```text
Status: PASSED
Steps: 3
Roles: product-owner → developer → qa
```

## Troubleshooting

- **"model did not return a JSON object"** — make sure the prompt explicitly
  asks for a JSON object and the model is `llama3.2:3b-fast`. Thinking models
  like `qwen3:4b` can still fill `max_tokens` with reasoning and never emit
  JSON; the harness strips `<think>` blocks when they are present, but it
  cannot invent an envelope that was never produced. See [Thinking models](#thinking-models).
- **Harness takes 3–6 minutes** — the model is generating long extra sections.
  This is usually a prompt issue, not Ollama. Verify the prompt says "ONLY 2
  specific copy improvements" and explicitly forbids additional sections.
- **10-minute timeout** — the wrong model is selected, `max_tokens` is too low
  for the prompt, or the model hit the timeout. Switch to `llama3.2:3b-fast`.
