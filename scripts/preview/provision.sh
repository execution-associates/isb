#!/usr/bin/env bash
# Run as root inside the preview VM (`mise run preview` does it). Installs
# what the isb inside needs: incus from Zabbly's stable channel (the channel
# titan runs), mise for rust and bun, and lingering for ubuntu's user units.
# Safe to run again.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

if ! command -v incus >/dev/null; then
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://pkgs.zabbly.com/key.asc -o /etc/apt/keyrings/zabbly.asc
  . /etc/os-release
  cat >/etc/apt/sources.list.d/zabbly-incus-stable.sources <<EOF
Enabled: yes
Types: deb
URIs: https://pkgs.zabbly.com/incus/stable
Suites: ${VERSION_CODENAME}
Components: main
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/zabbly.asc
EOF
  apt-get update -q
  apt-get install -y -q incus build-essential pkg-config clang
  incus admin init --minimal
fi
usermod -aG incus-admin ubuntu

if ! command -v mise >/dev/null; then
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://mise.jdx.dev/gpg-key.pub | gpg --dearmor -o /etc/apt/keyrings/mise-archive-keyring.gpg
  echo "deb [signed-by=/etc/apt/keyrings/mise-archive-keyring.gpg arch=$(dpkg --print-architecture)] https://mise.jdx.dev/deb stable main" \
    >/etc/apt/sources.list.d/mise.list
  apt-get update -q
  apt-get install -y -q mise
fi

loginctl enable-linger ubuntu
