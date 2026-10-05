#!/usr/bin/env bash
# Run as ubuntu inside the preview VM. Builds isb from the read-only checkout
# at /src into ~/.local/bin/isb (the target directory stays on the VM's own
# disk) and installs the web UI's dependencies.
set -euo pipefail

mise use -g --yes rust@stable bun@1 >/dev/null
mise trust --yes /src/mise.toml >/dev/null
cd /src
export CARGO_TARGET_DIR="$HOME/target"
mise exec -- cargo build --locked --bin isb
install -D -m 0755 "$CARGO_TARGET_DIR/debug/isb" "$HOME/.local/bin/isb"

bash /src/scripts/preview/node-modules.sh
cd /src/web
mise exec -- bun install --frozen-lockfile
