#!/usr/bin/env bash
# Set isb's version everywhere it is written down: the workspace crates,
# Cargo.lock's entries for them, both SDKs (and their lockfiles and tests)
# and the RPC reference. Usage: scripts/bump.sh major|minor|patch|X.Y.Z
#
# The release workflow refuses a tag that differs from Cargo.toml, and the
# SDK tests and docs quote the version the daemon reports, so a bump that
# misses one file fails later and further away. This edits every place and
# then checks that the old version is gone from all of them.
set -euo pipefail
cd "$(dirname "$0")/.."

old=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
IFS=. read -r ma mi pa <<<"$old"
case "${1:-}" in
  major) new="$((ma + 1)).0.0" ;;
  minor) new="$ma.$((mi + 1)).0" ;;
  patch) new="$ma.$mi.$((pa + 1))" ;;
  [0-9]*.[0-9]*.[0-9]*) new=$1 ;;
  *) echo "usage: $0 major|minor|patch|X.Y.Z" >&2; exit 2 ;;
esac
[ "$new" != "$old" ] || { echo "already $old" >&2; exit 1; }

o=${old//./\\.}
# Only the lines that carry isb's own version: other packages in these files
# can share the number (Cargo.lock's self_cell is 1.3.0 too).
sed -i -E "s/^version = \"$o\"/version = \"$new\"/; s/(isb-[a-z]+ = \{ path = \"crates\/isb-[a-z]+\", version = \"=)$o\"/\1$new\"/" Cargo.toml
# Cargo.lock: the version line right after a `name = "isb"` or `"isb-*"`.
sed -i -E "/^name = \"isb(-[a-z]+)?\"$/{n;s/^version = \"$o\"$/version = \"$new\"/}" Cargo.lock
sed -i -E "s/^version = \"$o\"$/version = \"$new\"/" sdk/python/pyproject.toml
sed -i -E "s/^__version__ = \"$o\"$/__version__ = \"$new\"/" sdk/python/src/isb/__init__.py
sed -i -E "/^name = \"isb-sdk\"$/{n;s/^version = \"$o\"$/version = \"$new\"/}" sdk/python/uv.lock
sed -i -E "s/\{\"isb\":\"$o\",/{\"isb\":\"$new\",/" sdk/python/tests/test_unit.py docs/reference/rpc.md
sed -i -E "s/\{\"isb\": \"$o\",/{\"isb\": \"$new\",/" docs/reference/rpc.md
sed -i -E "s/^  \"version\": \"$o\",$/  \"version\": \"$new\",/" \
  sdk/typescript/package.json sdk/typescript/npm/*/package.json
sed -i -E "s/(\"@execution-associates\/isb-linux-[a-z0-9]+\": )\"$o\"/\1\"$new\"/" \
  sdk/typescript/package.json sdk/typescript/bun.lock

files=(Cargo.toml sdk/python/pyproject.toml sdk/python/src/isb/__init__.py
  sdk/python/tests/test_unit.py docs/reference/rpc.md sdk/typescript/package.json
  sdk/typescript/npm/*/package.json)
left=$(grep -n -F "$old" "${files[@]}" || true)
left+=$(grep -A1 -E '^name = "isb(-[a-z]+)?"$' Cargo.lock | grep -F "\"$old\"" || true)
left+=$(grep -A1 '^name = "isb-sdk"$' sdk/python/uv.lock | grep -F "\"$old\"" || true)
left+=$(grep -F "isb-linux" sdk/typescript/bun.lock | grep -F "\"$old\"" || true)
if [ -n "$left" ]; then
  printf 'still %s:\n%s\n' "$old" "$left" >&2
  exit 1
fi
echo "$old -> $new"
