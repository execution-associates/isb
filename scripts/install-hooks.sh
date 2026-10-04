#!/usr/bin/env bash
# Point this repository (and its worktrees) at .githooks.
set -euo pipefail
cd "$(dirname "$0")/.."
git config core.hooksPath .githooks
echo "hooks: .githooks (pre-commit, pre-push; post-* chain to ~/.config/git/hooks)"
