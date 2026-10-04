#!/usr/bin/env bash
# The local pre-push check: the docs' links, then what CI's Rust jobs run,
# fastest first.
# Run it inside the dev sandbox. `scripts/check.sh --quick` stops after
# clippy (no tests); `scripts/check.sh --release` also builds the static
# musl release binary after the tests (scripts/build-release.sh, which needs
# musl-tools and the musl rust target and says what is missing).
set -euo pipefail
cd "$(dirname "$0")/.."
# Fail early, with the fix, when a prerequisite is missing.
[ "${1:-}" = --release ] && scripts/build-release.sh --check
set -x
scripts/check-docs.py
scripts/ratchet.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
[ "${1:-}" = --quick ] && exit 0
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
[ "${1:-}" = --release ] && scripts/build-release.sh
exit 0
