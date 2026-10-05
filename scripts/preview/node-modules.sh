#!/usr/bin/env bash
# Puts web/node_modules on the VM's own disk: bun links packages into it, and
# links are what virtiofs with a uid translation refuses (EIO). Run again
# after a reboot (isb-web.service does); a no-op once mounted.
set -euo pipefail
mnt=/src/web/node_modules
mountpoint -q "$mnt" && exit 0
local=/home/ubuntu/web-node_modules
mkdir -p "$local"
[ -d "$mnt" ] || { echo "$mnt is missing: mkdir -p web/node_modules on the host" >&2; exit 1; }
sudo mount --bind "$local" "$mnt"
