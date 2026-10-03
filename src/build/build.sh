#!/bin/bash
# One build, run by isb inside a fresh build sandbox. The source is in
# /build/src (copied in by isb), build arguments in /build/args (KEY=VALUE per
# line). The image is written to /build/out/image.tar as an OCI layout, which
# isb copies out and pushes; nothing here can reach the registry.
#
# Environment: ISB_BUILDER (dockerfile|railpack|nixpacks), ISB_CONTEXT (the
# directory to build, under /build/src), ISB_DOCKERFILE (relative to it),
# ISB_TARGET, ISB_RAILPACK_FRONTEND, ISB_CACHE_DISK (a VM's cache disk).
set -euo pipefail

say() { echo "isb-build: $*"; }

mkdir -p /build/out /build/plan /var/lib/buildkit

if [ -n "${ISB_CACHE_DISK:-}" ]; then
  # A VM gets its cache as a block volume: format it once, then reuse it.
  dev=""
  for d in /dev/disk/by-id/*"${ISB_CACHE_DISK}"; do
    [ -e "$d" ] && dev="$d" && break
  done
  [ -n "$dev" ] || { say "cache disk ${ISB_CACHE_DISK} not found"; ls /dev/disk/by-id; exit 1; }
  if ! blkid "$dev" >/dev/null 2>&1; then
    say "formatting the build cache"
    mkfs.ext4 -q -L isb-cache "$dev"
  fi
  mount "$dev" /var/lib/buildkit
fi

buildkitd --config /etc/buildkit/buildkitd.toml --root /var/lib/buildkit \
  --addr unix:///run/buildkit/buildkitd.sock >/build/buildkitd.log 2>&1 &
bk=$!
stop_buildkitd() {
  # A clean stop keeps the cache's metadata consistent.
  kill -TERM "$bk" 2>/dev/null || true
  for _ in $(seq 30); do kill -0 "$bk" 2>/dev/null || break; sleep 1; done
  sync
}
trap stop_buildkitd EXIT
for i in $(seq 61); do
  buildctl debug workers >/dev/null 2>&1 && break
  if [ "$i" -ge 60 ] || ! kill -0 "$bk" 2>/dev/null; then
    say "buildkitd did not start:"; tail -20 /build/buildkitd.log; exit 1
  fi
  sleep 1
done
say "$(tr '\n' ' ' </etc/isb-builder)"

ctx="${ISB_CONTEXT:-/build/src}"
bargs=(--progress plain --output "type=oci,dest=/build/out/image.tar")
pargs=()

# Build arguments: build-args for a Dockerfile; for railpack and nixpacks,
# environment (railpack hands them to the build as secrets).
if [ -s /build/args ]; then
  while IFS= read -r kv; do
    [ -n "$kv" ] || continue
    k="${kv%%=*}"
    case "$ISB_BUILDER" in
      dockerfile) bargs+=(--opt "build-arg:$kv") ;;
      railpack)
        pargs+=(--env "$kv")
        bargs+=(--secret "id=$k,env=$k")
        export "${kv?}" ;;
      nixpacks) pargs+=(--env "$kv") ;;
    esac
  done < /build/args
  if [ "$ISB_BUILDER" = railpack ]; then
    # Changed values must invalidate the layers that used them.
    bargs+=(--opt "build-arg:secrets-hash=$(sha256sum /build/args | cut -c1-64)")
  fi
fi

case "$ISB_BUILDER" in
  dockerfile)
    df="$ctx/${ISB_DOCKERFILE:-Dockerfile}"
    [ -f "$df" ] || { say "no Dockerfile at ${ISB_DOCKERFILE:-Dockerfile}"; exit 1; }
    bargs+=(--frontend dockerfile.v0 --local "context=$ctx"
      --local "dockerfile=$(dirname "$df")" --opt "filename=$(basename "$df")")
    [ -z "${ISB_TARGET:-}" ] || bargs+=(--opt "target=$ISB_TARGET")
    ;;
  railpack)
    say "railpack: planning"
    railpack prepare "$ctx" --plan-out /build/plan/railpack-plan.json \
      --info-out /build/plan/railpack-info.json "${pargs[@]}"
    bargs+=(--frontend gateway.v0 --opt "source=$ISB_RAILPACK_FRONTEND"
      --local "context=$ctx" --local dockerfile=/build/plan)
    ;;
  nixpacks)
    say "nixpacks: planning"
    # It writes .nixpacks/ (the Dockerfile and what it copies) into the
    # sandbox's copy of the source, which is then the context.
    nixpacks build "$ctx" --out "$ctx" "${pargs[@]}"
    bargs+=(--frontend dockerfile.v0 --local "context=$ctx"
      --local "dockerfile=$ctx/.nixpacks" --opt filename=Dockerfile)
    ;;
  *) say "unknown builder $ISB_BUILDER"; exit 2 ;;
esac

say "building"
buildctl build "${bargs[@]}"
say "image written ($(du -h /build/out/image.tar | cut -f1))"
