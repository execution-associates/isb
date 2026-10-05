#!/usr/bin/env bash
# Build the release binary the way release-binaries.yml does: the web UI, then a
# static musl `isb` (no libc on the host needed to run it).
#
#   scripts/build-release.sh              # for this machine's architecture
#   scripts/build-release.sh --check      # only check the prerequisites
#   TARGET=aarch64-unknown-linux-musl scripts/build-release.sh
#
# Prerequisites (docs/contributing/index.md#building-the-release-binary):
# rustup with the musl target, bun, and on Linux a musl C compiler
# (`musl-tools`, which provides musl-gcc). Run it inside the dev sandbox.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
Linux) TARGET=${TARGET:-$(uname -m)-unknown-linux-musl} ;;
Darwin) TARGET=${TARGET:-$(uname -m | sed s/arm64/aarch64/)-apple-darwin} ;;
*) echo "build-release.sh: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

missing=()
need() { command -v "$1" >/dev/null 2>&1 || missing+=("$2"); }
need cargo "cargo (mise use -g rust, or https://rustup.rs)"
need bun "bun (mise use -g bun)"
if command -v rustup >/dev/null 2>&1 &&
	! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
	missing+=("the Rust target: rustup target add $TARGET")
fi
case "$TARGET" in
*-linux-musl)
	# cc-rs finds no musl compiler by itself (ToolNotFound for
	# x86_64-linux-musl-gcc) unless CC_<target> names one.
	cc_var="CC_${TARGET//-/_}"
	if [ -z "${!cc_var:-}" ]; then
		if command -v musl-gcc >/dev/null 2>&1; then
			export "$cc_var=musl-gcc"
		else
			missing+=("a musl C compiler: apt install musl-tools (or set $cc_var)")
		fi
	fi
	;;
esac
if [ ${#missing[@]} -gt 0 ]; then
	printf 'build-release.sh: missing prerequisites:\n' >&2
	printf '  - %s\n' "${missing[@]}" >&2
	exit 1
fi
[ "${1:-}" = --check ] && { echo "prerequisites for $TARGET ok"; exit 0; }

set -x
(cd web && bun install --frozen-lockfile && bun run build)
ISB_WEB_REQUIRED=1 cargo build --release --locked --target "$TARGET"
bin=${CARGO_TARGET_DIR:-target}/$TARGET/release/isb
{ file "$bin" || ls -l "$bin"; } 2>/dev/null
"$bin" --version || echo "(not runnable on this machine)"
