#!/usr/bin/env bash
# Run as ubuntu (with a login session, for the incus-admin group) inside the
# preview VM, once its daemon is up. Makes the login dev@dev.com / password
# (a platform admin; ISB_DEV_WEAK_PASSWORDS lets the debug build take so
# short a password) and a `demo` org with something in each kind of service.
# Skips what already exists, but always resets dev@dev.com's password.
set -euo pipefail

isb=$HOME/.local/bin/isb
email=dev@dev.com

if $isb user ls | awk -v e="$email" '$2 == e { found = 1 } END { exit !found }'; then
  printf 'password\n' | ISB_DEV_WEAK_PASSWORDS=1 $isb user passwd "$email"
else
  printf 'password\n' | ISB_DEV_WEAK_PASSWORDS=1 $isb user create --admin "$email"
fi

# Five instances at one CPU each by default; the limits are bookkeeping.
$isb org create demo --cpus 12 --memory 12GiB
export ISB_ORG=demo

$isb project ls | grep -qw wiki || $isb project create wiki
$isb project env-add wiki staging 2>/dev/null || true
$isb db ls | grep -qw outline-db || $isb db create outline-db --project wiki --engine postgres

tmp=$(mktemp -d)
cat >"$tmp/wiki.yaml" <<'EOF'
services:
  web:
    image: docker:traefik/whoami:latest
    deploy: { replicas: 2 }
  redis:
    image: docker:valkey/valkey:8
EOF
cat >"$tmp/chat.yaml" <<'EOF'
services:
  chat:
    image: docker:traefik/whoami:latest
EOF
$isb stack deploy --file "$tmp/wiki.yaml" --project wiki --env production wiki
# No project: lands in a new project named after it.
$isb stack deploy --file "$tmp/chat.yaml" chat
rm -r "$tmp"

$isb project ls
