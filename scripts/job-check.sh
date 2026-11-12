#!/bin/sh
# What a nightshift job runs as its test command on this repo (`[test]` in
# nightshift.toml). It formats the code, then runs the same lint and tests as
# CI, so a job that ends "done" also passes the Format Check and Clippy jobs.
set -e
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
