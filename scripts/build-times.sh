#!/usr/bin/env bash
# Times the edit-compile loop: each cargo command, after a one-line edit in a
# leaf module (little depends on it) and in a hub module (much depends on it).
#
#   scripts/build-times.sh                     # check build test clippy
#   scripts/build-times.sh check release       # pick steps
#   RUNS=3 scripts/build-times.sh check        # best of 3 per cell
#   LEAF=path/to/a.rs HUB=path/to/b.rs scripts/build-times.sh
#
# Steps: check (cargo check), build (debug build of the binary), test
# (cargo test --no-run, every test target), clippy (--all-targets), release
# (release build for x86_64-unknown-linux-musl, minutes per cell).
#
# Each step is warmed once untimed, then every cell appends a fresh
# `const _: u32 = N;` to the file (a real code change, not a comment), times
# the command and restores the file (with a new mtime, so cargo rebuilds it). Prints a markdown table. Run it inside
# the dev sandbox, never on a host holding credentials.
set -euo pipefail
cd "$(dirname "$0")/.."

# The first path that exists, so the script survives files moving.
first() { for p in "$@"; do [ -f "$p" ] && { echo "$p"; return; }; done; echo "none of: $*" >&2; exit 1; }
LEAF=${LEAF:-$(first crates/isb-tui/src/ui.rs src/tui/ui.rs)}
HUB=${HUB:-$(first crates/isb-core/src/org.rs src/org.rs)}
RUNS=${RUNS:-1}
MUSL=${MUSL:-x86_64-unknown-linux-musl}

steps=("$@")
[ ${#steps[@]} -gt 0 ] || steps=(check build test clippy)

cmd_for() {
  case $1 in
    check) echo "cargo check --locked --workspace --all-targets" ;;
    build) echo "cargo build --locked --bin isb" ;;
    test) echo "cargo test --locked --workspace --no-run" ;;
    clippy) echo "cargo clippy --locked --workspace --all-targets" ;;
    release) echo "cargo build --locked --release --bin isb --target $MUSL" ;;
    *) echo "unknown step: $1" >&2; exit 2 ;;
  esac
}

backup=$(mktemp -d)
restore() {
  for f in "$LEAF" "$HUB"; do
    b="$backup/$(echo "$f" | tr / _)"
    [ -f "$b" ] && cp "$b" "$f"
  done
  rm -rf "$backup"
}
trap restore EXIT
for f in "$LEAF" "$HUB"; do cp -p "$f" "$backup/$(echo "$f" | tr / _)"; done

n=$(date +%s)
time_edit() { # time_edit FILE CMD -> seconds (best of RUNS)
  local f=$1 cmd=$2 best="" b t0 t1 s
  b="$backup/$(echo "$f" | tr / _)"
  for _ in $(seq "$RUNS"); do
    n=$((n + 1))
    cp "$b" "$f"
    printf '\nconst _: u32 = %d;\n' "$n" >>"$f"
    t0=$(date +%s.%N)
    $cmd >/dev/null 2>&1 || { echo "failed: $cmd" >&2; exit 1; }
    t1=$(date +%s.%N)
    s=$(awk -v a="$t0" -v b="$t1" 'BEGIN { print b - a }')
    if [ -z "$best" ] || awk -v s="$s" -v b="$best" 'BEGIN { exit !(s < b) }'; then best=$s; fi
  done
  cp "$b" "$f"
  printf '%.1f' "$best"
}

echo "leaf: $LEAF, hub: $HUB, runs: $RUNS, $(nproc) CPUs, $(rustc --version)" >&2
echo "| step | leaf edit (s) | hub edit (s) |"
echo "|---|---:|---:|"
for s in "${steps[@]}"; do
  cmd=$(cmd_for "$s")
  echo "warming: $cmd" >&2
  $cmd >/dev/null 2>&1 || { echo "failed: $cmd" >&2; exit 1; }
  leaf=$(time_edit "$LEAF" "$cmd")
  hub=$(time_edit "$HUB" "$cmd")
  echo "| $s | $leaf | $hub |"
done
