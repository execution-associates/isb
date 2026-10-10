#!/usr/bin/env bash
# Run as ubuntu inside the preview VM, after provision.sh. Turns the preview
# into the README's demo: an `acme` org with apps built from an image and
# from git, previews of two pull requests, Postgres with backups to S3 and a
# restore, secrets, domains, templates from isb's, Dokploy's and Coolify's
# catalogs, uptime monitors, a workspace reached over SSH, and an agent's
# token that has done some work. Everything is real: builds run, backups
# upload, monitors check.
#
# Run it on a fresh preview: it skips what exists, but a second run on top of
# a first adds deployments and runs to the history the screenshots show.
set -euo pipefail

isb=$HOME/.local/bin/isb
state=$HOME/readme-shots
mkdir -p "$state"
export ISB_ORG=acme
export XDG_RUNTIME_DIR=/run/user/$(id -u)
ip=$(ip -4 route get 1.1.1.1 | awk '{for (i = 1; i < NF; i++) if ($i == "src") print $(i + 1)}')

# A grep that reads all of its input: grep -q stops at the first match, and
# under pipefail the isb on the other end of the pipe then fails the test.
found() { grep "$@" >/dev/null; }
step() { printf '\n== %s\n' "$*"; }

step "daemon: local registry, ingress with an internal CA"
$isb registry setup >/dev/null 2>&1 || true
sudo $isb host setup --user ubuntu >/dev/null
env=$HOME/.config/isb/serve.env
ingress="ISB_INGRESS_HTTP=:80
ISB_INGRESS_HTTPS=:443
ISB_INGRESS_PUBLIC_IP=$ip
ISB_ACME_CA=internal"
# Restart only for a change: a restart fails the builds in flight.
if [ "$(grep '^ISB_INGRESS_\|^ISB_ACME_CA=' "$env")" != "$ingress" ]; then
  sed -i '/^ISB_INGRESS_\|^ISB_ACME_CA=/d' "$env"
  echo "$ingress" >>"$env"
  systemctl --user restart isb.service
fi
until curl -sf http://127.0.0.1:8092/healthz >/dev/null; do sleep 1; done

step "the dev@dev.com account and an agent's token"
if ! $isb user ls | awk '$2 == "dev@dev.com" { found = 1 } END { exit !found }'; then
  printf 'password\n' | ISB_DEV_WEAK_PASSWORDS=1 $isb user create --admin dev@dev.com
fi
$isb org ls | found -w acme || $isb org create acme --cpus 24 --memory 32GiB
if [ ! -s "$state/token" ]; then
  $isb token create claude-code --org acme --user dev@dev.com | tail -1 >"$state/token"
  chmod 600 "$state/token"
fi
tool() {
  curl -sf -H "Authorization: Bearer $(cat "$state/token")" -H 'Content-Type: application/json' \
    -d "$2" "http://127.0.0.1:8092/orgs/acme/api/v1/tools/$1"
}

# The default workspace image takes minutes: build it while the rest runs.
$isb workspace image ls | found '^isb-workspace ' || $isb workspace image build >"$state/workspace-image.log" 2>&1 &
image_build=$!

step "secrets"
secret() { $isb secret ls | awk -v n="$1" '$1 == n { f = 1 } END { exit !f }' || printf %s "$2" | $isb secret create "$1" - -l "$3"; }
secret stripe-api-key sk_live_51Hx9acmeDemoKey app=web
secret smtp-password demo-smtp-password app=web
secret session-key "$(openssl rand -hex 32)" app=api
secret github-token ghp_acmeDemoToken purpose=previews
secret s3-access-key acme purpose=backups
secret s3-secret-key acme-demo-secret-key purpose=backups

step "project shop: api from an image, Postgres, Redis"
$isb project ls | found -w shop || $isb project create shop
$isb project env-add shop staging 2>/dev/null || true
if ! $isb app ls | found -w '^api'; then
  $isb app create api --project shop --image docker:traefik/whoami -e LOG_LEVEL=info --replicas 3 -q
  # Plain HTTP: uptime checks trust only public CAs, and this ingress's
  # certificates come from its internal one.
  printf 'domains:\n  - {host: api.acme.dev, port: 80, https: false}\n' | $isb app update api -f - >/dev/null
  $isb app deploy api -q
fi
$isb db ls | found -w shop-db || $isb db create shop-db --project shop --engine postgres --database shop -q
$isb db ls | found -w cache || $isb db create cache --project shop --engine redis -q

step "web: built from git, with domains and pull request previews"
if ! $isb app ls | found -w '^web'; then
  $isb app create web --project shop --git https://github.com/traefik/whoami \
    --ref 3c6fc4814630c9645affe0e2ff42c8bc2d8aadae --builder dockerfile -q
  # The VM has no nested KVM for a VM build: build in a container.
  $isb app update web -f - >/dev/null <<'EOF'
port: 80
build: {untrusted: false}
domains:
  - {host: shop.acme.dev, port: 80, www_redirect: true}
  - {host: shop.acme.dev, path: /api, port: 80, strip_prefix: true}
  - {host: auto, port: 80}
previews:
  enabled: true
  branches: [master]
  max: 3
  domain: auto
  env: |
    MODE=preview
EOF
  printf '%s\n' 'STRIPE_API_KEY=${{secret.stripe-api-key}}' 'SMTP_PASSWORD=${{secret.smtp-password}}' \
    'DATABASE_URL=${{secret.db.shop-db.url}}' 'REDIS_URL=${{secret.db.cache.url}}' 'LOG_LEVEL=info' |
    $isb app env-set web - >/dev/null
  # One retry, for a download that fails on the way.
  $isb app deploy web -q || $isb app deploy web -q
  # The next deploy comes from GitHub: a push to master.
  printf 'source: {git: {ref: master, url: "https://github.com/traefik/whoami"}}\n' | $isb app update web -f - >/dev/null
fi
webhook_secret=$($isb app webhook web | sed -n 's/^secret: *//p')
hook() { # EVENT DELIVERY BODY
  local sig
  sig=$(printf %s "$3" | openssl dgst -sha256 -hmac "$webhook_secret" -hex | awk '{print $2}')
  curl -sf -H "X-GitHub-Event: $1" -H "X-GitHub-Delivery: $2" -H "X-Hub-Signature-256: sha256=$sig" \
    -H 'Content-Type: application/json' -d "$3" http://127.0.0.1:8092/api/v1/webhooks/acme/web
  echo
}
# The newest deployment's status and trigger, e.g. "done webhook".
latest() { $isb app deployments web | awk 'NR == 2 { print $2, $3 }'; }
for attempt in 1 2 3; do
  [ "$(latest)" = "done webhook" ] && break
  hook push "readme-push-$(date +%s)" '{"ref":"refs/heads/master","after":"1ce75d01b6978863647da42557a707a479da3a51","head_commit":{"message":"Fix release and updates"},"repository":{"full_name":"traefik/whoami"}}'
  sleep 2
  $isb app deploy-log web -f >/dev/null 2>&1 || true
done
[ "$(latest)" = "done webhook" ] || { echo "the push deployment did not succeed" >&2; exit 1; }
# The web app's own monitor would check https://shop.acme.dev against
# public CAs and never come up: leave it out.
tool monitor_delete '{"name":"app-web"}' >/dev/null || true
pr() { # NUMBER TITLE BRANCH SHA; real pull requests of traefik/whoami
  jq -cn --argjson n "$1" --arg t "$2" --arg r "$3" --arg s "$4" '{action: "opened", number: $n,
    pull_request: {number: $n, title: $t, merged: false,
      head: {ref: $r, sha: $s, repo: {full_name: "traefik/whoami", fork: false}},
      base: {ref: "master", repo: {full_name: "traefik/whoami"}}},
    repository: {full_name: "traefik/whoami"}}'
}
# One at a time, each retried: a failed download fails a whole build.
preview_state() { $isb app previews ls web | awk -v pr="#$1" '$2 == pr { print $3 }'; }
for spec in "84|Output the version number|dev|9447b0b75ba73409fe8f6a30e104bf8e5c629992" \
  "89|chore: update CI, dependencies, and linter|feat/update-ci|fba895b3b7c0a3888b0956dc59d94fb48f8d2f1b"; do
  IFS='|' read -r n title branch sha <<<"$spec"
  [ -n "$(preview_state "$n")" ] || hook pull_request "readme-pr-$n-$(date +%s)" "$(pr "$n" "$title" "$branch" "$sha")"
  for attempt in 1 2 3; do
    while [[ "$(preview_state "$n")" =~ ^(queued|building|deploying)$ ]]; do sleep 5; done
    [ "$(preview_state "$n")" = done ] && break
    sleep 30
    $isb app previews redeploy web "$n" -q || true
  done
  [ "$(preview_state "$n")" = done ] || { echo "preview #$n did not deploy" >&2; exit 1; }
done

step "backups to S3 (a local versitygw gateway), and a restore"
if ! $isb --org default stack ls | found -w s3; then
  cat >"$state/s3.yaml" <<'EOF'
services:
  gw:
    image: docker:versity/versitygw:latest
    entrypoint: ["/bin/sh", "-c", "mkdir -p /data/acme-backups && exec versitygw --access acme --secret acme-demo-secret-key posix /data"]
    ports: ["127.0.0.1:7070:7070"]
    volumes: ["data:/data"]
volumes:
  data: {}
EOF
  $isb --org default stack deploy s3 -q --file "$state/s3.yaml"
fi
$isb backup dest ls | found -w offsite ||
  $isb backup dest create offsite --endpoint http://s3.acme.internal:7070 --bucket acme-backups \
    --region us-east-1 --path-style --access-key-secret s3-access-key --secret-key-secret s3-secret-key
if ! $isb backup ls | found -w shop-db-nightly; then
  $isb backup create shop-db-nightly --database shop-db --destination offsite --schedule '0 3 * * *' --keep 14
  $isb backup run shop-db-nightly -q
  $isb backup run shop-db-nightly -q
  $isb backup restore shop-db-nightly --new shop-db-copy --project shop --environment staging
fi

step "templates: Dokploy's and Coolify's catalogs, and Umami from isb's"
$isb template catalog ls | found '^dokploy ' || $isb template catalog add dokploy https://templates.dokploy.com --format dokploy
$isb template catalog ls | found '^coolify ' ||
  $isb template catalog add coolify https://raw.githubusercontent.com/coollabsio/coolify/main --format coolify
$isb project ls | found -w analytics || $isb project create analytics
if ! $isb app ls | found -w '^umami'; then
  $isb template deploy builtin/umami --project analytics >/dev/null
  printf 'domains:\n  - {host: auto, https: false}\n' | $isb app update umami -f - >/dev/null
  $isb app deploy umami -q
fi

step "uptime monitors (every 30 s, so a few minutes make a history)"
for m in app-api app-umami; do tool monitor_update "{\"name\":\"$m\",\"interval\":30}" >/dev/null || true; done
tool monitor_get '{"name":"status-page"}' >/dev/null ||
  tool monitor_create '{"name":"status-page","type":"http","url":"https://www.githubstatus.com/","interval":30}' >/dev/null
tool monitor_get '{"name":"docs"}' >/dev/null ||
  tool monitor_create '{"name":"docs","type":"http","url":"https://docs.github.com/","interval":30}' >/dev/null

step "the agent's work: a sandbox it made and ran tests in"
if ! $isb ls 2>/dev/null | found -w agent-tests; then
  tool sandbox_create '{"spec":{"container_name":"agent-tests","image":"images:ubuntu/24.04","labels":{"owner":"claude-code"}},"wait_ready":true}' >/dev/null
  tool sandbox_exec '{"name":"agent-tests","command":["bash","-lc","uname -sr && python3 --version"]}' >/dev/null
  tool stack_status '{"name":"shop-production"}' >/dev/null
  tool app_deployments '{"name":"web"}' >/dev/null
fi

step "the workspace, with the isb CLI, reached over SSH"
wait "$image_build" || { cat "$state/workspace-image.log"; exit 1; }
$isb workspace ls 2>/dev/null | found '^workspace ' || $isb workspace create
# One line: sudo -i runs it through a login shell, which joins lines. The
# loop waits for a just-made workspace's DNS.
incus exec --project isb-acme workspace -- sudo -iu dev bash -c 'for i in $(seq 30); do [ -x ~/.local/bin/isb ] && break; curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh || sleep 5; done; [ -x ~/.local/bin/isb ] && { grep -q local/bin ~/.bashrc || echo "export PATH=\$HOME/.local/bin:\$PATH" >>~/.bashrc; }' </dev/null
[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -q -t ed25519 -N '' -f ~/.ssh/id_ed25519 -C dev@acme
$isb key ls --user dev@dev.com 2>/dev/null | found -w laptop ||
  $isb key add ~/.ssh/id_ed25519.pub --user dev@dev.com --name laptop
$isb ssh-config acme/workspace --user dev --as dev@dev.com -o ~/.config/isb/ssh_config -q
touch ~/.ssh/config && chmod 600 ~/.ssh/config
grep -q 'Include ~/.config/isb/ssh_config' ~/.ssh/config || echo 'Include ~/.config/isb/ssh_config' >>~/.ssh/config
ssh -o BatchMode=yes workspace.acme.isb true

# When the monitors started: the uptime page's soak counts from the first run.
[ -s "$state/seeded" ] || date +%s >"$state/seeded"
step "seeded"
