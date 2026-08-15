#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW_DIR="$REPO_ROOT/.github/workflows"
offending=0

while IFS=: read -r file line content; do
  [[ "$content" =~ ^[[:space:]]*# ]] && continue
  if [[ ! "$content" =~ uses:[[:space:]]*([^[:space:]#]+) ]]; then
    continue
  fi

  reference="${BASH_REMATCH[1]}"
  reference="${reference#\"}"
  reference="${reference%\"}"
  reference="${reference#\'}"
  reference="${reference%\'}"
  [[ "$reference" == ./* ]] && continue
  if [[ ! "$reference" =~ ^[^@]+@[0-9a-fA-F]{40}$ ]]; then
    printf '%s:%s: unpinned action reference: %s\n' "$file" "$line" "$reference" >&2
    offending=1
  fi
done < <(grep -RIn --include='*.yml' --include='*.yaml' 'uses:' "$WORKFLOW_DIR" || true)

if (( offending != 0 )); then
  exit 1
fi

printf 'All workflow action references are pinned.\n'
