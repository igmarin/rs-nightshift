#!/usr/bin/env bash
set -euo pipefail

# Runs the pre-push gate subset:
#   cargo fmt --all -- --check,
#   cargo clippy --all-targets --all-features -- -D warnings,
#   cargo test, cargo test --doc, cargo audit, cargo deny check
#
# This intentionally does NOT run rs-guard; that is a maintainer workflow.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

missing=()
if ! command -v cargo >/dev/null 2>&1; then
  missing+=("cargo")
fi

# rustfmt and clippy must be available (they ship with the rustup toolchain).
for component in fmt clippy; do
  if ! cargo "$component" --version >/dev/null 2>&1; then
    missing+=("cargo-$component")
  fi
done

# cargo-audit and cargo-deny are required for this gate; we provide a helpful
# error if they are missing.
for sub in audit deny; do
  if ! cargo "$sub" --version >/dev/null 2>&1; then
    missing+=("cargo-$sub")
  fi
done

if ((${#missing[@]} > 0)); then
  printf 'Missing required tools: %s\n' "${missing[*]}" >&2
  printf 'Install them with:\n\n  mise install\n\n' >&2
  printf 'or:\n\n  cargo binstall cargo-audit@0.22.2 cargo-deny@0.20.2 -y\n\n' >&2
  exit 1
fi

cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --doc
cargo audit
cargo deny check

printf 'Pre-push gate passed.\n'
