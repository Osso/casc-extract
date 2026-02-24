#!/bin/bash
set -euo pipefail

echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy ==="
cargo clippy -- -D warnings

echo "=== cargo test ==="
cargo test

echo "All checks passed."
