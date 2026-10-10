#!/usr/bin/env bash
# Run as ubuntu inside the preview VM, after seed.sh. Drives three terminal
# scenes in tmux and keeps each screen with its colors, rendered to a page
# shoot.py screenshots: `isb up` serving a directory, herdr attached to the
# workspace over isb's SSH, and `isb tui` in the middle of a rolling update
# (last, so its restart shows in no other scene).
set -euo pipefail

isb=$HOME/.local/bin/isb
dir=$HOME/readme-shots
term=$dir/term
mkdir -p "$term"
export XDG_RUNTIME_DIR=/run/user/$(id -u)
render() { mise exec -- uv run -q /src/scripts/readme-shots/ansi2html.py "$term/$1.ansi" "$term/$1.html" "$2"; }
keys() { tmux send-keys -t "$1" "$2" Enter; sleep "${3:-1}"; }
tmux kill-server 2>/dev/null || true

# isb up: a sandbox from the README's quick start, outside every org.
demo=$HOME/demo
mkdir -p "$demo/site"
echo '<h1>hello from a sandbox</h1>' >"$demo/site/index.html"
cat >"$demo/isb.yaml" <<'EOF'
services:
  web:
    image: images:ubuntu/24.04
    idmap: auto
    volumes: ["./site:/home/ubuntu/site"]
    ports: ["8000:8000"]
    user: ubuntu
    working_dir: /home/ubuntu/site
    command: python3 -u -m http.server 8000
EOF
# Once without the camera, so the capture shows a sandbox made from the
# cached image rather than a download.
(cd "$demo" && env -u ISB_ORG $isb up -d -q && env -u ISB_ORG $isb down -q)
tmux new-session -d -s up -x 96 -y 26 -c "$demo" "env -u ISB_ORG PS1='\[\e[1;35m\]~/demo\[\e[0m\] \$ ' bash --norc -i"
keys up clear
keys up 'cat isb.yaml'
keys up 'isb up' 1
until curl -sf -o /dev/null localhost:8000/; do sleep 1; done
for p in / /index.html /; do curl -s -o /dev/null "localhost:8000$p"; sleep 0.4; done
sleep 1
tmux capture-pane -t up -e -p >"$term/sandbox.ansi"
tmux send-keys -t up C-c
render sandbox "~/demo — isb up"

# herdr on this machine, attached to the workspace's herdr through
# `ssh workspace.acme.isb` (isb ssh-proxy over the daemon's websocket).
tmux new-session -d -s hd -x 146 -y 22 "bash --norc -i"
keys hd 'eval "$(mise activate bash)"; clear'
keys hd 'herdr --remote workspace.acme.isb' 12
# A first attach shows herdr's welcome and its integrations panel.
tmux send-keys -t hd Enter; sleep 2
tmux send-keys -t hd Escape; sleep 1
keys hd 'exec bash -l' 7   # the remote session outlives runs; a fresh login shell has the PATH and the token
tmux send-keys -t hd C-l; sleep 1
keys hd 'isb app top api' 4
keys hd 'isb db ls' 3
tmux capture-pane -t hd -e -p >"$term/ssh.ansi"
render ssh "herdr --remote workspace.acme.isb"

# isb tui: shop-production while api rolls out, slot 1 replaced and slot 2
# being created.
tmux new-session -d -s tui -x 118 -y 32 "$isb tui"
sleep 6
tmux send-keys -t tui Down   # stacks are sorted: analytics-production, then shop-production
sleep 25                     # a few CPU samples for the header's sparkline
ISB_ORG=acme $isb app restart api -q &
restart=$!
frame=
for _ in $(seq 1 40); do
  sleep 1.5
  tmux capture-pane -t tui -e -p >"$term/tui.ansi"
  plain=$(sed 's/\x1b\[[0-9;]*m//g' "$term/tui.ansi")
  if grep -q 'slot 1 .*retired.*serving' <<<"$plain" && grep -q 'slot 2 .*creating' <<<"$plain"; then
    frame=1
    break
  fi
done
wait "$restart"
[ -n "$frame" ] || { echo "terminals: never caught api mid-rollout" >&2; exit 1; }
render tui "isb tui"

tmux kill-server
echo "terminals: $term"
