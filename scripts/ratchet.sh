#!/usr/bin/env bash
# The legibility ratchet, in milliseconds. Two budgets, both in
# scripts/ratchet.txt, that may go down but never up:
#
#  - File size: no Rust file may exceed LIMIT lines, except the files listed
#    (`path lines`), each capped at its listed size. Once a listed file is
#    under LIMIT it must leave the list.
#  - Lint exemptions: clippy enforces the thresholds in clippy.toml, and an
#    existing offender carries an `#[allow(clippy::<lint>, reason = ...)]`.
#    The number of times each ratcheted lint is named in the source
#    (`lint:<name> count`) may not grow, so new code cannot opt out quietly.
#
#   scripts/ratchet.sh            # check (CI and scripts/check.sh run this)
#   scripts/ratchet.sh --update   # lower budgets to today's numbers; never raises
#
# Raising a budget is a deliberate edit to scripts/ratchet.txt that a
# reviewer sees. Splitting the file or function is almost always better.
set -euo pipefail
cd "$(dirname "$0")/.."

LIMIT=1000
LINTS="too_many_lines cognitive_complexity excessive_nesting too_many_arguments type_complexity"
BUDGET=scripts/ratchet.txt
update=false
[ "${1:-}" = --update ] && update=true

# Every tracked or new .rs file (or every one outside target/ without git).
if git rev-parse --git-dir >/dev/null 2>&1; then
  list=$(git ls-files -co --exclude-standard -- '*.rs')
else
  list=$(find . -name '*.rs' -not -path './target/*' -not -path '*/node_modules/*' | sed 's|^\./||')
fi
files=()
while read -r f; do [ -f "$f" ] && files+=("$f"); done < <(sort -u <<<"$list")

# `path lines` for every file, then `lint:<name> count` for every lint.
measure() {
  wc -l "${files[@]}" | awk '$2 != "total" { print $2, $1 }'
  for l in $LINTS; do
    printf 'lint:%s %s\n' "$l" "$(cat "${files[@]}" | grep -o "clippy::$l\b" | wc -l)"
  done
}
now=$(measure)

out=$(awk -v limit="$LIMIT" -v update="$update" '
  FNR == NR { if ($0 !~ /^#/ && NF == 2) budget[$1] = $2; next }
  { size[$1] = $2 }
  END {
    bad = 0
    verb = update == "true" ? "lower" : "note"
    for (f in size) {
      n = size[f]
      if (f ~ /^lint:/) {
        b = (f in budget) ? budget[f] : 0
        if (n > b) { printf "FAIL %s: named %d times, budget %d (fix the function instead of allowing the lint)\n", f, n, b; bad = 1 }
        else if (n < b) printf "%s %s: named %d times, budget %d\n", verb, f, n, b
      } else if (f in budget) {
        if (n > budget[f]) { printf "FAIL %s: %d lines, budget %d (split it)\n", f, n, budget[f]; bad = 1 }
        else if (n <= limit) { printf "%s %s: %d lines, under the %d-line limit: drop it from the budget\n", (update == "true" ? "drop" : "FAIL"), f, n, limit; if (update != "true") bad = 1 }
        else if (n < budget[f]) printf "%s %s: %d lines, budget %d\n", verb, f, n, budget[f]
      } else if (n > limit) {
        printf "FAIL %s: %d lines, over the %d-line limit (split it)\n", f, n, limit; bad = 1
      }
    }
    for (f in budget) if (!(f in size)) { printf "%s %s: in the budget but gone\n", (update == "true" ? "drop" : "FAIL"), f; if (update != "true") bad = 1 }
    exit bad
  }' "$BUDGET" - <<<"$now") && status=0 || status=$?
[ -z "$out" ] || sort <<<"$out"

if $update; then
  {
    sed -n '/^#/p' "$BUDGET"
    awk -v limit="$LIMIT" '
      FNR == NR { if ($0 !~ /^#/ && NF == 2) budget[$1] = $2; next }
      ($1 ~ /^lint:/ || (($1 in budget) && $2 > limit)) {
        b = ($1 in budget) ? budget[$1] : 0
        print $1, ($2 < b ? $2 : b)
      }' "$BUDGET" - <<<"$now" | sort
  } >"$BUDGET.new"
  mv "$BUDGET.new" "$BUDGET"
  echo "updated $BUDGET"
  exit 0
fi
[ "$status" = 0 ] && echo "ratchet: ok (${#files[@]} files)"
exit "$status"
