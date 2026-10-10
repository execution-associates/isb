#!/usr/bin/env bash
# Run as root inside the preview VM (`mise run readme:shots` does it), after
# `mise run preview` made it. Adds what the README's demo needs on top of the
# preview: git and skopeo for builds, an ingress on 80/443, names for the demo
# domains, tmux and fonts for the terminal captures, and Chromium for the
# screenshots. Safe to run again.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get install -y -q git skopeo tmux jq openssl webp fonts-dejavu-core >/dev/null

# The terminal dashboard's header names the host.
hostnamectl set-hostname isb-host
grep -q ' isb-host$' /etc/hosts || echo "127.0.1.1 isb-host" >>/etc/hosts

isb=/home/ubuntu/.local/bin/isb
"$isb" host setup --user ubuntu --public-ingress >/dev/null

# The demo's domains resolve to this VM's ingress; the backup bucket is the
# S3 gateway seed.sh publishes on loopback.
ip=$(ip -4 route get 1.1.1.1 | awk '{for (i = 1; i < NF; i++) if ($i == "src") print $(i + 1)}')
sed -i '/# isb readme-shots$/d' /etc/hosts
echo "$ip shop.acme.dev www.shop.acme.dev api.acme.dev # isb readme-shots" >>/etc/hosts
echo "127.0.0.1 s3.acme.internal # isb readme-shots" >>/etc/hosts

# uv for the Python scripts, herdr for terminals.sh, and Chromium's
# libraries for shoot.py's Playwright.
sudo -iu ubuntu mise use -g --yes uv@latest herdr@latest >/dev/null
uvx=$(sudo -iu ubuntu mise which uvx)
"$uvx" --from playwright==1.55.0 playwright install-deps chromium >/dev/null
