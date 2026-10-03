#!/bin/sh
# The recipe for isb's builder image: run as root in a fresh
# images:ubuntu/24.04 instance (container or VM) in the isb-system project,
# which isb then publishes as `isb-builder/<hash of this file>`. Changing this
# file makes a new image on the next build.
#
# Every download is pinned to a version and a SHA-256 checksum taken from the
# upstream release (railpack publishes checksums.txt; for BuildKit and
# nixpacks the checksum was recorded when the version was pinned).
set -eu

BUILDKIT_VERSION=v0.33.1
BUILDKIT_SHA256=4e044bcd62a0c0bbe6a8c94d73989de2bfe4c04dbc0f9d6021cf96b72cd1d965
RAILPACK_VERSION=v0.40.1
RAILPACK_SHA256=2842de93e68713af9037e0bc0a398d7da78f3b96aa4804303a638db2bc69bd30
NIXPACKS_VERSION=v1.41.0
NIXPACKS_SHA256=0f55de7874507b9cf7502113120bd96f2ab6979f78d10eaf2eb2ade9207b3af6

export DEBIAN_FRONTEND=noninteractive
# The network may take a moment after boot.
i=0
until getent hosts github.com >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { echo "no DNS after 60s" >&2; exit 1; }
  sleep 1
done
apt-get update -q
apt-get install -y -q --no-install-recommends ca-certificates curl tar gzip e2fsprogs

fetch() { # url sha256 file
  curl -fsSL --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum -c -
}

cd /tmp
fetch "https://github.com/moby/buildkit/releases/download/${BUILDKIT_VERSION}/buildkit-${BUILDKIT_VERSION}.linux-amd64.tar.gz" \
  "$BUILDKIT_SHA256" buildkit.tgz
tar -xzf buildkit.tgz -C /usr/local bin/buildkitd bin/buildctl bin/buildkit-runc
fetch "https://github.com/railwayapp/railpack/releases/download/${RAILPACK_VERSION}/railpack-${RAILPACK_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
  "$RAILPACK_SHA256" railpack.tgz
tar -xzf railpack.tgz -C /usr/local/bin railpack
fetch "https://github.com/railwayapp/nixpacks/releases/download/${NIXPACKS_VERSION}/nixpacks-${NIXPACKS_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
  "$NIXPACKS_SHA256" nixpacks.tgz
tar -xzf nixpacks.tgz -C /usr/local/bin nixpacks
rm -f buildkit.tgz railpack.tgz nixpacks.tgz

mkdir -p /etc/buildkit /var/lib/buildkit
# The cache volume is per app; keep it bounded.
cat > /etc/buildkit/buildkitd.toml <<'EOF'
[worker.oci]
  enabled = true
  gc = true
  gckeepstorage = "10GB"
[worker.containerd]
  enabled = false
EOF

cat > /etc/isb-builder <<EOF
buildkit=${BUILDKIT_VERSION}
railpack=${RAILPACK_VERSION}
nixpacks=${NIXPACKS_VERSION}
EOF

buildkitd --version
railpack --version
nixpacks --version

apt-get clean
rm -rf /var/lib/apt/lists/* /var/log/apt /tmp/* /root/.cache
# A published image starts with a fresh identity.
truncate -s 0 /etc/machine-id 2>/dev/null || true
sync
