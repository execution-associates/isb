#!/usr/bin/env bash
# The local pre-push check: what CI's Rust jobs run, fastest first.
# Run it inside the dev sandbox. `scripts/check.sh --quick` stops after
# clippy (no tests).
set -euo pipefail
cd "$(dirname "$0")/.."
set -x
scripts/ratchet.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
[ "${1:-}" = --quick ] && exit 0
cargo test --workspace --locked
