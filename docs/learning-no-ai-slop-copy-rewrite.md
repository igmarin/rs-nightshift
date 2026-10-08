# No AI Slop Copy Rewrite — Learning Experience

## Context

Goal: rewrite all user-facing copy on [ismaelmarin.dev](https://ismaelmarin.dev) to remove generic AI slop (buzzwords, vague claims, corporate filler) and replace it with specific, human, evidence-backed writing.

The task was run through the `rs-nightshift` harness — a multi-role LLM pipeline (Product Owner → Developer → QA) that uses local Ollama models on a CPU-only system.

## What happened

### Attempt 1: llama3.2:3b

- **Speed**: ~3 tok/s
- **Result**: Produced placeholder text like `"change from '...' to '...'"` instead of quoting real file content
- **Verdict**: Too small to follow instructions. Could not produce specific, actionable output

### Attempt 2: qwen3:4b

- **Speed**: ~2 tok/s
- **Result**: Spent all tokens (3072 max) on reasoning preamble. Never produced JSON output
- **Fix tried**: Added `/no_think` directive to suppress reasoning — did not work through Ollama's OpenAI-compatible API
- **Verdict**: Thinking model architecture incompatible with the harness's JSON output requirement. The model reasons instead of answering

### Attempt 3: llama3.1:8b (winner)

- **Speed**: ~2 tok/s (prompt processing ~15-20 min, generation ~20-40 min per role)
- **Result**: PO role produced an excellent brief in ~20 min. Developer and QA roles completed successfully on small files
- **Verdict**: This is the sweet spot for quality on CPU-only hardware. Non-thinking model, good instruction following, produces valid JSON

## What we fixed in rs-nightshift

Four changes were made to the harness during this process:

### 1. Backtick template literal repair (parser)

**Problem**: Small models sometimes emit JavaScript template literals (backticks) instead of standard JSON strings for the `content` field.

**Fix**: Added `repair_backtick_strings` to `extract_json_object` pipeline in `src/application/executor.rs`. Converts backtick-delimited strings to double-quoted JSON strings.

### 2. Timeout increased to 60 minutes

**Problem**: Default generate timeout was 10 minutes. CPU inference with 32k context takes 15-20 min for prompt processing alone.

**Fix**: Increased default generate timeout in `src/adapters/ollama.rs` from 600s → 3600s.

### 3. 3-way merge fallback for apply-patch

**Problem**: `git apply --check` fails when model produces patches with wrong context lines. The existing hunk-header repair only fixes line counts, not content.

**Fix**: Added `apply_3way` fallback in `src/adapters/git.rs`. When strict check and hunk repair both fail, tries `git apply --3way` which uses the index for 3-way merge — more tolerant of context-line mismatches.

### 4. Context file limit raised to 64 KiB

**Problem**: `MAX_FILE_BYTES` was 8 KiB. `public/index.html` is 33 KB — truncated by the limit.

**Fix**: Increased `MAX_FILE_BYTES` in `src/application/executor.rs` from 8192 → 65536.

## The real bottleneck: tools, not model size

The core problem is not model intelligence — it's the available tools:

| Tool | How it works | Where it fails |
|------|-------------|----------------|
| `apply-patch` | Model produces unified diff, harness runs `git apply` | Models can't track line numbers across large files. Context lines don't match. Patches are structurally malformed |
| `write-file` | Model outputs complete file content | Works for files <10 KB on CPU. Times out for larger files because generation is too slow (~2 tok/s) |
| **Missing** | Search-and-replace: "old text → new text" pairs | This is what aider uses. Models are good at quoting specific text but bad at line numbers |

### What works on CPU-only systems

- **PO role (analysis/briefs)**: Works great with 8b. Produces specific, actionable briefs in ~20 min
- **Small files (<10 KB) with `write-file`**: Works. Model reproduces the file with changes in ~17 min
- **Large files**: Must be applied manually from the PO brief. No tool in nightshift can handle this autonomously yet

### What doesn't work

- `apply-patch` with any model size on large files — the diff format is too fragile
- `write-file` on files >10 KB on CPU — generation too slow, hits 60-min timeout
- Thinking models (qwen3) — spend all tokens reasoning, never produce JSON
- Models <4B — can't follow instructions well enough to quote real text

## Model size comparison

| Model | Size | Speed (CPU) | Quality | Thinking? | Works? |
|-------|------|-------------|---------|-----------|--------|
| llama3.2:3b | 2.0 GB | ~3 tok/s | Placeholder output | No | No — too small |
| qwen3:4b | 2.5 GB | ~2 tok/s | Never produced JSON | Yes | No — thinking model |
| llama3.1:8b | 4.9 GB | ~2 tok/s | Good briefs, valid JSON | No | **Yes** |
| gemma3:12b | 8.1 GB | ~0.5-1 tok/s | Highest quality | No | Maybe — too slow for 60-min timeout |

## Key takeaways

1. **Model size is not the bottleneck** — the 8b model produces good output. The bottleneck is tool design
2. **Non-thinking models are required** — thinking models (qwen3) waste tokens on reasoning and never produce structured output through Ollama's OpenAI API
3. **`OLLAMA_KEEP_ALIVE=24h` is critical** — prevents cold starts between roles. Without it, each role reloads the model (~2-3 min penalty)
4. **Temperature 0.2-0.3** — low temperature for Developer/QA (deterministic), slightly higher for PO (creative but grounded)
5. **`max_tokens` matters** — PO needs 2048-3072 (briefs), Developer needs 4096-8192 (file content), QA needs 1024-2048 (reports)
6. **Process one file at a time for large repos** — smaller context window, faster prompt processing, more reliable output
7. **The harness is best used as an analysis tool** — let the PO role produce the brief, then apply changes manually or with the harness for small files

## Configuration that worked

```toml
[providers.ollama]
base_url = "http://127.0.0.1:11434"

# PO role: temperature 0.3, max_tokens 2048-3072
# Developer role: temperature 0.2, max_tokens 4096 (write-file)
# QA role: temperature 0.2, max_tokens 1024-2048
```

Ollama Modelfile:
```
FROM llama3.1:8b
PARAMETER num_ctx 32768
PARAMETER num_thread 8
PARAMETER num_gpu 0
PARAMETER temperature 0.2
```

## Future improvements

See GitHub issues for planned improvements:
- `search-replace` tool (the missing piece for large files)
- Prompt engineering for thinking models
- Chunked write-file for large files
- Model benchmarking suite for harness compatibility
