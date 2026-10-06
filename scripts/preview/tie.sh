#!/usr/bin/env bash
# Runs on the host, from `mise run preview` and `preview:ui`: ties the preview
# to the agent that brought it up. When that agent's process exits, its
# tailnet URL goes off and the VM stops (`preview:ui` starts it again;
# `preview:down` deletes it), so a finished session leaves no superadmin UI
# open on the tailnet and no VM holding 8 CPUs.
#
# The agent is PREVIEW_OWNER_PID, or else the nearest `claude` ancestor. With
# neither (a person at a shell), nothing is tied and the preview stays up.
# The watcher is a transient systemd user unit, not a child of the task, so
# it fires however the agent goes: an exit, a closed pane, a SIGKILL.
set -euo pipefail
name="isb-preview-$USER"
unit="$name-owner"

owner=${PREVIEW_OWNER_PID:-}
if [ -z "$owner" ]; then
  p=$PPID
  while [ "$p" -gt 1 ]; do
    if [ "$(cat "/proc/$p/comm" 2>/dev/null)" = claude ]; then owner=$p; break; fi
    p=$(awk '/^PPid:/ {print $2}' "/proc/$p/status" 2>/dev/null || echo 1)
  done
fi

# A rerun moves the tie to whoever ran it last.
systemctl --user stop "$unit" 2>/dev/null || true
if [ -z "$owner" ]; then
  echo "preview: not tied to an agent; \`mise run preview:down\` removes it"
  exit 0
fi
systemd-run --user --quiet --collect --unit="$unit" \
  --description="Stop $name when process $owner exits" \
  bash -c "while kill -0 $owner 2>/dev/null; do sleep 10; done
    tailscale serve --https=$PREVIEW_PORT off || true
    incus stop -f $name </dev/null || true"
echo "preview: stops when process $owner ($(cat "/proc/$owner/comm")) exits"
