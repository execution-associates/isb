#!/usr/bin/env bash
# Run as ubuntu inside the preview VM. Builds isb from the read-only checkout
# at /src into ~/.local/bin/isb (the target directory stays on the VM's own
# disk) and installs the web UI's dependencies.
#
# The build is ra-jit's (github.com/indexable-inc/ra-jit): the first one is
# cargo's, and once its daemon has loaded the workspace a rebuild after an
# edit compiles only the functions that changed, in about a second where
# cargo takes 10 to 20. ra-jit works with Rust 1.99.0 only, so the preview
# pins it; on any other toolchain every build is plain cargo's. The daemon
# holds about 10 GB and exits after 30 minutes without a build. ra-jit 0.1.0
# has no borrow checker, so an edit cargo rejects can build here: the final
# word stays scripts/check.sh and CI, which are plain cargo.
set -euo pipefail

mise use -g --yes rust@1.99.0 bun@1 >/dev/null
mise trust --yes /src/mise.toml >/dev/null
mise exec -- rustup component add rust-src >/dev/null
# A few minutes the first time; after that cargo sees it installed and returns.
mise exec -- cargo install --locked --quiet ra-jit@0.1.0
cd /src
export CARGO_TARGET_DIR="$HOME/target"
mise exec -- cargo ra-jit build --locked --bin isb
install -D -m 0755 "$CARGO_TARGET_DIR/debug/isb" "$HOME/.local/bin/isb"

bash /src/scripts/preview/node-modules.sh
cd /src/web
mise exec -- bun install --frozen-lockfile
