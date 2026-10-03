# TASKS

The work plan for turning isb into a multi-tenant platform: orgs as the trust
boundary, built-in auth, pluggable secrets, a deploy loop, a web UI, day-2
operations and remote servers. On Linux and macOS. Dokploy is the UX bar; its
Apache-2.0 code is a reference for requirements, never copied (nothing under
any `proprietary/` directory is read at all). `notes/dokploy-requirements.md`
condenses what it does.

## How to use this file

- One task per line. Status: `[ ]` todo, `[~]` in progress (with an owner:
  `[~] (agent-name)`), `[x]` done (with the commit), `[!]` blocked (say why).
- Claim a task before starting it; update it when you stop, done or not.
- A task is done when its **Verify** step passed, not when the code compiles.
- Keep notes short and factual. Decisions go under **Decisions**, with the
  reason, so nobody re-litigates them.

## Ground rules

- Branch `platform`, off `main`. Never commit to `main` directly (it
  auto-pushes and publishes). Subagents work in their own worktree branches and
  hand back a commit to merge.
- Cargo, bun, builds and tests run in an isb sandbox, never on the host
  (~/.claude/CLAUDE.md). Integration tests that need incusd are compiled in the
  sandbox (`cargo test --no-run`) and run on the host.
- No secret values in commits, logs, PRs or this file.
- Code style: match the crate. Terse why-comments, every wait bounded, errors
  that name the step.
- Docs describe the current state; update them with the feature, not after.

## Verification hosts

| Host | For | Notes |
|---|---|---|
| titan (Linux, incus 7.5) | integration tests, daemon | the dev host |
| minime (macOS 27, arm64, 16 GB) | macOS support: `isb machine`, CLI, TUI, web UI in Safari/Chrome | `ssh minime`; installs go through mise or brew, and anything installed for testing is listed here |
| hcloud EU box (Linux, cheapest that runs incus) | a clean-install target: installer, remote server, public ingress + ACME, federation | `hcloud` with `HCLOUD_TOKEN`; label `owner=isb-platform`; delete when idle |
| browser | web UI end to end | lasso's shared browser (titan-local URLs) or minime-chrome (tailnet/public URLs); close every page opened |

Installed for testing: (none yet)

**minime disk is tight: 21 GiB free after a cleanup (2026-10-03).** Keep the
Lima VM disk ≤ 10 GiB for tests, delete test VMs and downloaded binaries when
done. Stephan may free more (Downloads 6G, Claude desktop vm_bundles 10G).

**Nothing untrusted builds on minime** (it holds Stephan's Apple session):
macOS binaries are built and unit-tested by GitHub Actions macOS runners;
minime only runs binaries downloaded from our CI runs.

## Decisions

- **Org = trust boundary = incus project.** An org's people and agents fully
  administer the org: apps, secrets (read included), deploys. Nothing crosses
  orgs. Platform admins span orgs.
- **Identity: built-in.** Email + password, passkeys, OAuth (GitHub, Google)
  and generic OIDC SSO, invitations, API tokens. Cloudflare Access stays as an
  optional front door, mapped to users.
- **Secrets: pluggable drivers, Swarm UX.** Stacks hold references, never
  values; a version change is a new revision (rolling update). Drivers:
  `local` (age-encrypted, default), inline `age:` (safe in git),
  `onepassword`, later Vault/Infisical/Bitwarden. `file:`/`environment:`
  sources become `local` secrets named `<stack>_<name>` on deploy, as swarm
  does. Env delivery (`{secret: name}`) is an extension; on OCI images it lands
  in incus config (documented trade-off).
- **The daemon's age key:** lookup env `ISB_AGE_KEY` → systemd credential →
  `ISB_AGE_KEY_FILE` → `~/.config/isb/age.txt`. `isb serve install` makes an
  encrypted systemd credential where supported, else the file (and says to
  exclude it from backups). Warn without a break-glass recipient.
- **macOS:** incus runs only on Linux, so isb on a Mac drives incus inside a
  Lima VM it manages (`isb machine init|start|stop|rm|status`, like
  `podman machine`), with the socket forwarded and home directories shared.
  The daemon can run on the Mac against that socket.
- **One API, three surfaces.** Tools are defined once (the MCP registry); a
  REST/JSON API and SSE event stream are generated from the same registry;
  the web UI, TUI, CLI and agents all use it.
- **Web UI:** React + Vite + Tailwind + shadcn, built with bun, embedded in
  the binary; served by `isb serve`.
- **Org runtime, measured on incus 7.5 (2026-10-03):** a restricted project
  (`restricted=true`, unprivileged containers, `restricted.devices.disk=allow`
  with `restricted.devices.disk.paths` = the org's bind roots,
  `restricted.networks.access` = the org bridge, `restricted.idmap.uid/gid` =
  the daemon user) makes incus itself refuse host paths outside the roots,
  proxy devices, privileged, nesting, `raw.lxc`, `raw.idmap` of root, other
  networks, and exceeding the org quota. With project `limits.cpu/memory`
  set, every instance needs its own limits, so the org's default profile
  carries defaults (like a LimitRange). `security.idmap.isolated=true` gives
  each instance its own uid range. `features.images=false` shares the host's
  images.
- **Org network:** one bridge per org, `isbbr<hash>` (IFNAMSIZ is 15), with
  `dns.domain=<org>.isb`, plus a per-org ACL (deny private ranges except its
  own subnet). Default-deny host firewalls (ufw on titan) drop DHCP/DNS/egress
  on new bridges, so `sudo isb host setup` installs wildcard allows once
  (`ufw allow in on isbbr+` 67/udp, 53; `ufw route allow in on isbbr+ out on
  <uplink>`); ufw's routed default-deny keeps org bridges apart.
- **Shared types:** `isb::org::OrgId` (validated name, `incus_project()`,
  `dir(state)`) is the key every org-scoped module uses.
- **Remote servers: federation, not incus clustering.** Each server runs incus
  and `isb serve` as an agent; the control plane places orgs on servers and
  proxies to them over mTLS.

## Phase 0: platform support

- [x] (816a9fc, 6bee7c1) P0.1 macOS build: make the crate compile and its unit tests pass on
  macOS (gate `/proc`-based code: foreground ancestor polling via `sysctl`/
  `libproc` equivalents or `getppid` chains, host metrics via `sysctl`, peer
  credentials via `getpeereid`). CI: add macOS to the release matrix
  (aarch64-apple-darwin, x86_64-apple-darwin). **Verify:** `cargo test` on
  minime.
- [ ] P0.1b npm darwin packages: `sdk/typescript/npm/darwin-{arm64,x64}`,
  optionalDependencies + `os`, `platformPackage()` in `src/binary.ts`, darwin
  rows in `sdk-typescript.yml`. bun.lock needs the packages published first
  (or regenerated). PyPI darwin wheels are in the workflow, untested until a tag.
- [ ] P0.2 `isb machine`: Lima-backed incus VM on macOS (init with CPU/memory/
  disk, start, stop, rm, status, ssh), incus installed from Zabbly, socket
  forwarded to `~/.isb/machine/<name>/incus.sock`, `$HOME` shared at the same
  path (so bind mounts work), default socket discovery uses it. Published
  ports reachable from the Mac's localhost. **Verify:** on minime, from
  nothing: `isb machine init && isb up` of the README example, port reachable
  from macOS; `isb tui` works.
- [ ] P0.3 `isb serve` on macOS: launchd agent install (`isb serve install`
  writes a LaunchAgent plist), balancer listening on the Mac, reaching
  replicas in the VM. **Verify:** a 2-replica stack on minime, curl from
  macOS spreads across both.

## Phase 1: foundation

### Orgs
- [~] (orchestrator) P1.1 Org model: `isb org create|ls|rm|show`, an org = incus project
  `isb-<org>` created restricted (no privileged, managed disks only,
  limits from the org's quota), per-org network (bridge) and default ACLs.
  Every command, tool and the daemon take `--org` (default: a `default`
  org mapped to the incus `default` project for backwards compatibility).
  Stacks and sandboxes live inside their org. **Verify:** two orgs, a stack
  in each, neither can see or reach the other (exec, list, network).
- [ ] P1.2 Per-org network policy: allow within the org, deny across orgs and
  to private ranges by default, named exceptions in the org config.
  **Verify:** curl across orgs fails, within succeeds, egress to internet ok.
- [ ] P1.3 Service discovery: stable names per service inside an org
  (`<service>.<stack>.isb` or similar), resolving to the instance (1 replica)
  or an org-local balancer address (replicas). **Verify:** an app reaches its
  postgres by name through a rolling replacement of the postgres.

### Identity and API
- [~] (subagent p1.4) P1.4 Users and sessions: built-in store (SQLite in the state dir),
  argon2id passwords, sessions with secure cookies, first-run admin setup,
  invitations, roles (platform admin; org admin/member), API tokens (hashed,
  org-scoped). **Verify:** unit tests + login over HTTP.
- [ ] P1.5 OAuth/OIDC: GitHub, Google, generic OIDC (discovery, PKCE),
  account linking by verified email. **Verify:** GitHub login end to end in a
  browser against the hcloud box.
- [ ] P1.6 Passkeys (WebAuthn): register and sign in. **Verify:** browser on
  minime (Touch ID or a virtual authenticator via CDP).
- [ ] P1.7 REST + SSE API generated from the tool registry: `/api/v1/<tool>`,
  OpenAPI document, `/api/v1/events` SSE, auth by session or token; MCP keeps
  working; per-org MCP endpoint `/orgs/<org>/mcp` bound to the caller's org.
  Cloudflare Access identities map to users. **Verify:** the same call via
  MCP, REST and CLI; an org token cannot touch another org.

### Secrets
- [~] (subagent p1.8) P1.8 age store + driver trait + `isb secret create|set|get|ls|inspect|rm|
  encrypt|reencrypt|refresh`, per org; daemon key lookup and generation;
  break-glass recipients. **Verify:** unit tests; reencrypt round trip with a
  second recipient.
- [ ] P1.9 Stacks reference secrets by name+version; revision uses versions;
  migrate existing stack state (base64 values) into the local store on daemon
  start. **Verify:** existing e2e stack survives the upgrade; a `secret set`
  rolls the dependent service.
- [ ] P1.10 Inline `age:` secrets in compose, `{secret: name}` env delivery
  (unit env file; OCI incus config), external-driver refresh polling.
  **Verify:** integration test for each delivery path.
- [ ] P1.11 `onepassword` driver (via `op` service account token stored as a
  local secret, or titan's broker). **Verify:** against a titan vault.
- [ ] P1.12 Secret tools on MCP/REST (`secret_list|get|set|delete`), org-scoped;
  `isb serve install` creates the systemd credential (Linux) / keychain-backed
  file (macOS). **Verify:** an org agent rotates a secret over MCP and the
  service rolls.

## Phase 2: the deploy loop

- [ ] P2.1 Git sources: GitHub App (install, repo list, webhooks), generic git
  over HTTPS/SSH with deploy keys, GitLab/Gitea webhooks. **Verify:** push to
  a test repo deploys on the hcloud box.
- [ ] P2.2 Builds in sandboxes: Railpack (and Nixpacks), Dockerfile,
  buildpacks; each build in a fresh isb sandbox (VM for untrusted), logs
  streamed, build cache volume per app. **Verify:** a Node, a Python and a
  Dockerfile app build and run.
- [ ] P2.3 Local OCI registry as an isb service; builds push, incus pulls;
  image retention. **Verify:** deploy pulls from the local registry; rollback
  to a previous image.
- [ ] P2.4 Ingress: embedded edge proxy (Caddy) with ACME, `domains:` per
  service (host, path, port, https, redirects), generated hostnames
  (sslip.io-style) for quick starts; Cloudflare Tunnel as an alternative
  provider per org. **Verify:** HTTPS on the hcloud box with a real cert.
- [ ] P2.5 App model + env editor: "application" (git/image source, build
  settings, env, domains, volumes, replicas) as a first-class object over
  stacks; project → environment → service hierarchy. **Verify:** create, edit,
  deploy and redeploy an app through the API.

## Phase 3: web UI

- [ ] P3.1 Scaffold: React + Vite + Tailwind + shadcn, bun build, embedded in
  the binary, served by `isb serve`; auth pages (sign in with SSO, GitHub,
  Google, email/password, passkey; create account; lost password).
  **Verify:** sign-in flows in the browser.
- [ ] P3.2 Dashboard + org/project/environment navigation; service pages with
  tabs: General, Environment, Domains, Deployments, Logs, Monitoring,
  Advanced; live updates over SSE. **Verify:** browser walkthrough recorded
  with screenshots.
- [ ] P3.3 Deploy flows: new app from git/image/template, deploy with live
  build logs, rollback, scale, web terminal (xterm.js over websocket exec).
  **Verify:** end to end in the browser.
- [ ] P3.4 Org admin: members, invitations, roles, API tokens, secrets editor,
  settings. **Verify:** invite a second user and sign in as them.

## Phase 4: day 2

- [ ] P4.1 Database templates (Postgres, MySQL/MariaDB, Redis, MongoDB) with
  credentials as secrets and scheduled backups to S3-compatible destinations;
  restore. **Verify:** backup and restore a Postgres on the hcloud box.
- [ ] P4.2 Scheduled jobs (cron) per service/org. **Verify:** a job runs on
  schedule and its logs are visible.
- [ ] P4.3 Notifications (Slack, Discord, Telegram, email, webhook) on deploy,
  failure, health, backup events. **Verify:** a webhook receives events.
- [ ] P4.4 Template catalog (one-click apps). **Stretch goal:** running Dokploy's
  templates (docker-compose + `template.toml`: variables, domains, mounts)
  directly; check the Dokploy/templates repo license before shipping its
  catalog. Only if it fits isb's architecture without bending it.
  **Verify:** deploy two native (and, if done, two Dokploy) templates from the UI.
- [ ] P4.5 Preview deployments per pull request. **Verify:** a PR on the test
  repo gets a URL; closing it removes it.
- [ ] P4.6 Metrics history (retained samples) and monitoring pages.

## Phase 5: scale-out

- [ ] P5.1 Remote servers: add a server (SSH bootstrap installs incus + isb
  agent), mTLS between control plane and agent, health. **Verify:** the
  hcloud box joins titan's (or a test) control plane.
- [ ] P5.2 Placement: orgs on servers; MCP/REST calls proxied to the owning
  server; secrets delivered only to servers that run their consumers.
  **Verify:** deploy to an org placed on the hcloud box from titan's UI.
- [ ] P5.3 Audit log and finer roles. **Verify:** actions appear in the log
  with the acting user or agent.

## Log

- 2026-10-03: P1.1 core landed (org create/ls/show/rm, --org, host setup);
  stacks and the daemon are not org-aware yet (next).

- 2026-10-03: plan written; branch `platform` off main at 0.7.0.
