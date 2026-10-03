#!/bin/sh
# isb's default workspace image (docs/guides/workspace-images.md), built by
# `isb workspace image build isb-workspace` from images:ubuntu/24.04. It runs
# as root in a throwaway container and must work unattended.
#
#   - a `dev` user at uid 1000 with passwordless sudo (the workspace user);
#   - openssh-server, installed but not running: isb's SSH starts `sshd -i`
#     per connection, and each workspace makes its own host keys;
#   - git, build-essential, curl and the usual command-line tools;
#   - mise, with node (LTS), bun and uv installed system-wide;
#   - Claude Code, Codex and herdr.
#
# Everything lives outside /home, so a workspace's home (a volume or a host
# folder mounted over /home/dev) never shadows it. No credentials are baked
# in: agents sign in from inside the workspace, or read the org's secrets.
set -eu

export DEBIAN_FRONTEND=noninteractive
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

say() { printf '== %s\n' "$*"; }

say "waiting for the network"
i=0
until getent hosts archive.ubuntu.com >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { echo "no network after 60s" >&2; exit 1; }
  sleep 1
done

say "packages"
apt-get update -q
apt-get install -yq --no-install-recommends \
  ca-certificates curl wget gnupg git build-essential pkg-config \
  openssh-server sudo unzip zip xz-utils zstd python3 python3-venv \
  jq less nano vim-tiny iproute2 procps psmisc rsync file \
  bash-completion locales tzdata man-db ripgrep tmux

# sshd stays off: isb runs `sshd -i` per connection. Host keys are made per
# workspace on its first SSH connection (ssh-keygen -A), never shared
# through the image.
systemctl disable ssh.service ssh.socket >/dev/null 2>&1 || true
mkdir -p /run/sshd

say "the dev user (uid 1000)"
existing=$(getent passwd 1000 | cut -d: -f1 || true)
if [ -n "$existing" ] && [ "$existing" != dev ]; then
  usermod -l dev -d /home/dev -m "$existing"
  groupmod -n dev "$existing" 2>/dev/null || true
elif [ -z "$existing" ]; then
  useradd -m -u 1000 -U -s /bin/bash dev
fi
usermod -s /bin/bash -aG sudo dev
printf 'dev ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/90-dev
chmod 0440 /etc/sudoers.d/90-dev

arch=$(uname -m)
case "$arch" in
  x86_64 | amd64) arch=x86_64 claude_platform=linux-x64 ;;
  aarch64 | arm64) arch=aarch64 claude_platform=linux-arm64 ;;
  *) echo "unsupported architecture $arch" >&2; exit 1 ;;
esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Fetch a URL to a file, then check it against a SHA-256.
fetch() {
  curl -fsSL --retry 3 --connect-timeout 20 --max-time 600 "$1" -o "$2"
}
verify() {
  actual=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$actual" = "$2" ] || { echo "checksum mismatch for $1" >&2; exit 1; }
}

say "mise (system-wide: /usr/local/bin/mise, tools in /usr/local/share/mise)"
curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh
mkdir -p /etc/mise
cat > /etc/mise/config.toml <<'EOF'
# System-wide tools, installed with `mise install --system` in the image.
# `mise use -g TOOL` as the workspace user adds more under the home.
[tools]
node = "lts"
bun = "latest"
uv = "latest"
EOF
MISE_YES=1 mise install --system
MISE_DATA_DIR=/usr/local/share/mise mise reshim
cat > /etc/profile.d/mise.sh <<'EOF'
# mise's system-wide tools (the image's), for login shells and scripts.
case ":$PATH:" in
  *:/usr/local/share/mise/shims:*) ;;
  *) export PATH="$PATH:/usr/local/share/mise/shims" ;;
esac
EOF
if ! grep -q 'mise activate bash' /etc/bash.bashrc; then
  printf '\n# mise (system-wide and per-user tools)\neval "$(/usr/local/bin/mise activate bash)"\n' >> /etc/bash.bashrc
fi

say "Claude Code (the native build, from downloads.claude.ai, checksum verified)"
base=https://downloads.claude.ai/claude-code-releases
version=$(curl -fsSL "$base/latest")
fetch "$base/$version/manifest.json" "$tmp/claude.json"
sum=$(jq -r --arg p "$claude_platform" '.platforms[$p].checksum // empty' "$tmp/claude.json")
[ -n "$sum" ] || { echo "Claude Code $version has no $claude_platform build" >&2; exit 1; }
fetch "$base/$version/$claude_platform/claude" "$tmp/claude"
verify "$tmp/claude" "$sum"
install -m 0755 "$tmp/claude" /usr/local/bin/claude
# The image owns the binary: updates come with a rebuilt image.
cat > /etc/profile.d/claude-code.sh <<'EOF'
export DISABLE_AUTOUPDATER=1
EOF

say "Codex (the GitHub release, checksum from the release's digest)"
asset="codex-$arch-unknown-linux-musl.tar.gz"
fetch https://api.github.com/repos/openai/codex/releases/latest "$tmp/codex.json"
url=$(jq -r --arg a "$asset" '.assets[] | select(.name == $a) | .browser_download_url' "$tmp/codex.json")
sum=$(jq -r --arg a "$asset" '.assets[] | select(.name == $a) | .digest // empty' "$tmp/codex.json")
sum=${sum#sha256:}
[ -n "$url" ] && [ -n "$sum" ] || { echo "no $asset with a digest in Codex's latest release" >&2; exit 1; }
fetch "$url" "$tmp/codex.tar.gz"
verify "$tmp/codex.tar.gz" "$sum"
tar -xzf "$tmp/codex.tar.gz" -C "$tmp"
install -m 0755 "$tmp/codex-$arch-unknown-linux-musl" /usr/local/bin/codex

say "herdr (its official installer, which verifies the download)"
curl -fsSL https://herdr.dev/install.sh | HERDR_INSTALL_DIR=/usr/local/bin sh

say "versions"
printf 'mise %s\n' "$(mise --version)"
su - dev -c 'node --version; bun --version; uv --version' 2>&1
printf 'claude %s\n' "$(/usr/local/bin/claude --version 2>&1 | head -1)"
printf '%s\n' "$(/usr/local/bin/codex --version 2>&1 | head -1)"
printf '%s\n' "$(/usr/local/bin/herdr --version 2>&1 | head -1)"

say "cleaning up"
apt-get clean
rm -rf /var/lib/apt/lists/* /root/.cache /tmp/* /var/tmp/*
rm -f /etc/ssh/ssh_host_*
truncate -s 0 /etc/machine-id 2>/dev/null || true
