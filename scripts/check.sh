#!/usr/bin/env bash
# Runs the workspace checks in order: formatting, clippy (warnings are errors), tests.
# Stops at the first failing step and exits with that step's exit code.

set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "All checks passed."
