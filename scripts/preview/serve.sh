#!/usr/bin/env bash
# Run as ubuntu inside the preview VM, after build.sh and `isb host setup`.
# (Re)starts the two user services: isb serve on 127.0.0.1:8092, and vite on
# :5173 proxying to it. PREVIEW_URL is the https URL the tailnet serves the
# UI at; passkeys and the daemon's Host check both want it.
#
# The daemon is a debug build, so it honours the dev switches: weak
# passwords always (dev@dev.com / password), and, unless
# PREVIEW_AUTH=password, ISB_DEV_SUPERADMIN, which signs every browser in as
# superadmin dev@dev.com with no sign-in page.
set -euo pipefail

: "${PREVIEW_URL:?PREVIEW_URL is the tailnet URL, e.g. https://titan.example.ts.net:8510}"
host="${PREVIEW_URL#https://}"
host="${host%%:*}"
export XDG_RUNTIME_DIR="/run/user/$(id -u)"
units="$HOME/.config/systemd/user"
mkdir -p "$units" "$HOME/.config/isb"
chmod 0700 "$HOME/.config/isb"

umask 077
{
  echo "ISB_SERVE_LISTEN=127.0.0.1:8092"
  echo "ISB_PUBLIC_URL=$PREVIEW_URL"
  echo "ISB_DEV_WEAK_PASSWORDS=1"
  if [ "${PREVIEW_AUTH:-superadmin}" != password ]; then
    echo "ISB_DEV_SUPERADMIN=dev@dev.com"
  fi
} >"$HOME/.config/isb/serve.env"
umask 022

"$HOME/.local/bin/isb" serve install

cat >"$units/isb-web.service" <<EOF
[Unit]
Description=isb web UI (vite, hot reload)
After=isb.service

[Service]
WorkingDirectory=/src/web
Environment=ISB_PREVIEW_HOST=$host
ExecStartPre=/usr/bin/bash /src/scripts/preview/node-modules.sh
ExecStart=/usr/bin/mise exec -- bun run dev
Restart=on-failure

[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable isb-web.service >/dev/null
systemctl --user restart isb.service isb-web.service
