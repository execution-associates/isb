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
| isb-test (hcloud cx23, nbg1, 2.28.124.127) | Stephan's standing test box: incus 7.5.1 (Zabbly stable), isb at `/usr/local/bin/isb`, `isb host setup` applied, ufw SSH-only | `ssh isb-test` (root); herdr machine `isb-test`; tailnet `isb-test.tail9dd8e.ts.net:8192` (tailnet superadmin); public `https://isb-test.execution.associates` through Cloudflare (EXA account: tunnel `isb-test` 14ac70ec-c3e7-4f72-9f0f-621082636701 run by `cloudflared-isb.service`, proxied CNAME, Access app `isb-test` 5ed14d80-a368-4288-9476-53728c8c6214 with Managed OAuth, allow + `--superadmin-access` knowsuchagency@gmail.com); delete those three with the box; kept, not deleted when idle |
| browser | web UI end to end | lasso's shared browser (titan-local URLs) or minime-chrome (tailnet/public URLs); close every page opened |

Installed for testing: lima 2.2.0 on minime (`brew install lima`).

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
- **Superadmin = the unix socket's reach, from four sources only:** the
  socket, superadmin tokens, `--superadmin-tailnet`, `--superadmin-access`.
  Superadmin tokens are minted by the host CLI (which writes `isb.db` as the
  daemon's user) and never over HTTP, so a stolen HTTP credential cannot
  become a durable one. Tailnet and Access identities are ambient (like a
  cookie), so they get the CSRF/Origin/Content-Type/Host checks.
  `Caller::is_trusted` means superadmin; `is_local` is the literal socket
  (deploy triggers labelled `manual`, `sandbox_create` resolving relative
  paths in the daemon's cwd).
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
  `podman machine`), with the socket forwarded and `$HOME` shared at the same
  path. `isb serve` runs **inside the VM** (next to the bridges its balancer
  reaches), with its socket and HTTP port forwarded to the Mac; `isb serve
  install` on macOS is a LaunchAgent that keeps the machine running.
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
- **Service names cost dnsmasq its AppArmor profile.** Names come from a
  per-org hosts directory (`/var/lib/isb/dns/<org>`, made by `isb host
  setup`) wired in with `raw.dnsmasq=hostsdir=`, and incus runs a bridge's
  dnsmasq unconfined once `raw.dnsmasq` is set. It still runs as `incus`.
  Accepted for now; an opt-out per org is the fallback.
- **Phase 2 contracts** (so P2.1/P2.5, P2.2/P2.3 and P2.4 can run in
  parallel):
  - `isb::build::{Builder, BuildRequest, BuiltImage, run}` (crates/isb-apps/src/build/mod.rs)
    is the one call the app layer makes to turn a checkout into an image.
    Its owner may add fields, never rename these.
  - A compose service takes `domains:` (list of `{host, path?, port,
    https?, redirect?}`; `https` defaults true; `path` defaults `/`). The
    ingress owns parsing and serving it; the app layer only emits it.
  - The app layer renders apps to ordinary stacks: an org's project +
    environment is one stack (`<project>-<env>`), each app one service in
    it, so service names (`<app>.<project>-<env>`) work between apps.
- **Web UI design system** (docs/contributing/web-ui.md#design-system): status colours only
  through `lib/status.ts`; Inter and JetBrains Mono (OFL 1.1, Latin subsets)
  self-hosted, since the CSP allows no font CDN. A deploy opens its
  deployment in the same frame (record seeded in the query cache) and
  follows it with `app_deployment_log`, which returns the record with the
  text; the deployment log also carries the controller's rollout events.
  Measured on titan (debug build): click to first log line 76-260 ms.
- **Web UI is embedded by `build.rs` as an `include_bytes!` table** (no
  crate). Without `web/dist` the binary serves a placeholder;
  `ISB_WEB_REQUIRED=1` (set by releases) makes that a build error, so plain
  `cargo build` never needs bun. The UI uses only the public API (REST
  tools, `/api/v1/auth/*`, SSE). CSP forbids inline scripts. Tokens in links
  ride in the URL fragment.
- **Apps** (P2.5): environments live inside the project record; app names
  are unique per org; one deploy per app at a time, a newer request
  supersedes a waiting one; rollback restores the deployment's image digest
  and service settings, not the saved app; image sources are pinned to a
  digest at deploy; apps take named volumes only; start-first rollouts, or
  stop-first with volumes; deployment log lines go on the events feed at
  level `log`.
- **Webhooks bypass sessions and Access in the daemon** (`public_routes`),
  authenticated by per-app HMAC or token. Behind Cloudflare Access the
  Access app needs a bypass policy for `/api/v1/webhooks/*`.
- **Ingress** (P2.4): Caddy (pinned 2.11.6, SHA-512 compiled in) is a child
  of `isb serve`, admin API on a unix socket, whole config reloaded on every
  change; never adopted across daemon restarts; `install_trust: false`
  always (it once wrote its root CA into titan's trust store; removed).
  Hostname claims persist with grant time, first claim wins. cloudflared
  runs inside the org (never on the host, where a dashboard-edited tunnel
  could reach host loopback such as the incus socket), reaching Caddy on a
  per-org listener at the org's bridge address :8480. Domains are left out
  of the service revision, so editing them never replaces instances.
- **Org settings that keep orgs apart are platform-admin only** (limits,
  egress, create/delete; `org_*` tools). Org owners see them read-only. Bind
  roots and the domain allowlist/ingress provider are set only by `isb org
  create` (host paths and cross-org claims). `org_delete` is refused while
  stacks are deployed. Members may read their org's secret values (the org
  is the trust boundary); the UI hides Reveal from members as a nicety only.
- **Agents sharing this session's scratchpad use their own subdirectory**
  (`scratchpad/<task>/`): one agent deleted another's script there.
- **Builds** (P2.2) run in the org's own project as unprivileged
  containers (BuildKit works there without `security.nesting`, so projects
  stay nesting-blocked and builds count against the org's quota and ACL);
  `untrusted` always means a VM. The builder image is prepared only in
  `isb-system`, where no org can tamper with it.
- **Registry** (P2.3): one per host in the `isb-system` project, no NIC, a
  unix socket inside plus a proxy on host 127.0.0.1:5480, so no org network
  can reach it. Only the daemon pushes (OCI push in Rust; it never trusts
  build output). `registry:APP[:TAG]` resolves in the caller's org, so
  cross-org references are impossible; loopback `oci:` refs are refused.
  Tags are pinned to digests at deploy (`StackDef.images`); retention keeps
  deployed digests with `isb-keep-*` tags. No registry auth: host-local
  processes can read and write every org's images.
- **The deploy loop works end to end on titan** (2026-10-03): a Dockerfile
  app from a `git://` repo built in its org (175 s cold, incl. the builder
  image), pushed to the local registry, deployed, served v1; a GitLab-style
  webhook deployed v2; rollback to v1 took 19 s with no rebuild.
- **Phase 4 contract: event kinds.** `Event.kind` (`<subject>.<outcome>`,
  listed on the type) is what notifications match on; producers emit with
  `Controller::event(kind, level, stack, service, message)`. A new kind is
  added to the list on `Event::kind` by whoever emits it.
- **Web terminal** is a websocket at `/orgs/<org>/api/v1/terminal`, admitted
  as `sandbox_exec` in that org (same org rules and tool policy;
  `--deny-tools sandbox_exec` turns it off); cookie upgrades need an Origin
  matching Host. 16 sessions, 30 min idle, 8 h max.
- **Notifications** (P4.3): per-org channels with secrets by name; the
  dispatcher follows the in-memory event ring from the start of each daemon
  run (nothing persisted, so nothing re-sent; queued deliveries are lost on
  restart). Private/loopback targets are refused unless a platform admin
  allows them server-wide (`notification_settings`); every resolved and
  connected address is checked, redirects never followed. SMTP is a minimal
  client over rustls. `health.unhealthy` fires after 20 s without a healthy
  replica, only for services healthy once in this run, never mid-rollout.
- **Metrics history** (P4.6): SQLite per org (`orgs/<org>/metrics.db`), tiers
  10 s/24 h, 1 min/7 d, 10 min/30 d; ~20 MiB for 20 instances over 31 days.
- **Previews** (P4.5) run in their own stack `<project>-<env>-pr-<n>` with
  their own volumes, build cache and `pr-<n>-<sha>` tags, and start with an
  empty env (`inherit_env` opts in) so they never point at production's data
  by default. Fork PRs are off by default; when allowed they build in a VM,
  get no app secrets (only `fork_secrets`) and a per-PR cache, since a PR
  could otherwise poison the cache production builds read.
- **Known flaky test:** `balance::tests::changing_listen_moves_the_route`
  failed once in a P4.5 run, then passed three times. Investigate if seen
  again.
- **Templates** (P4.4) instantiate as apps (so app pages, deploys and
  rollback work for them); generated values are org secrets
  `tpl.<name>.<var>`, derived values their own secrets. Dokploy's catalog
  (MIT) is fetched at runtime as an added catalog, not bundled. The
  translator refuses rather than weakens isolation (privileged, caps,
  devices, host namespaces/paths, docker socket); of 532 blueprints, 151
  translate cleanly, 250 with notes, 131 are refused. Apps gained `files`,
  `user` and `working_dir`; previews take `files` only with `inherit_env`,
  forks only `fork_secrets`' (a fix made at merge).
- **Same-org TCP needs `isb host setup`'s bridged-forward rule** on hosts
  with `br_netfilter` (titan: k3s, Docker): ufw's routed default-deny
  dropped all but ICMP between an org's instances. Fixed 2026-10-03
  (4c297ae); `orgs_isolate` now checks TCP, not only ping.
- **`dev-base` is built by lasso's `scripts/dev-base.sh`** (Ubuntu 24.04,
  `dev` uid 1000, mise: node, go, bun, uv); `--force` rebuilds it. The
  stopped `dev-base` container is its source.
- **Databases** (P4.1) are apps with a `database` source; credentials are
  org secrets `db.<n>.*` that outlive the database (the engine reads them
  only when its data directory is first made). Dumps run inside the
  database's own instance; the daemon streams them to S3 (SigV4 in Rust), so
  the org needs no route to the bucket. Loopback/link-local backup
  endpoints only from the local CLI or a platform admin.
- **Scheduler** (P4.2): one thread for jobs and backups; cron is isb's own
  parser, UTC or a fixed offset only; a missed slot runs once at startup if
  within `missed_grace` (1h).
- **isb keeps a persistent history of everything** (Stephan, 2026-10-03):
  the controller's events, every incus lifecycle event in every project
  (including changes made outside isb, with incus's requestor), and the
  audit rows, in one append-only, hash-chained store, queryable per object
  (`isb history <name>`), so "how did we get here" and "who deleted this"
  are one query. Gaps while no daemon ran are recorded as markers, never
  silent. Built as part of P5.3.
- **Audit log** (P5.3): its own append-only, hash-chained SQLite file
  (`audit.db`, triggers refuse edits), recorded at the tool-dispatch hook so
  every tool is covered; refusals always recorded; error messages never
  stored (they can quote argument values). New `viewer` role and opt-in
  token scopes (`read`, `deploy`, `admin`, `tool:GLOB`) that only narrow a
  role. A tool can name an argument that makes one call a secret read
  (`isbSecretReadArg`, e.g. `database_get`'s `reveal`).
- **History** lives in `audit.db` beside the audit rows, on its own hash
  chain: every controller event, every incus lifecycle event in every
  project (with requestor; context scrubbed of secrets), and markers
  (`serve.started/stopped`, `incus.gap`). Repeated exec/file reads are
  folded to one row per instance, program and requestor per hour (isb's own
  probes made ~48 rows/min per stack otherwise); ~1,800 rows/day on titan,
  ~1.5 KB/row, 365 days or 5M rows. Members see their org's controller and
  incus rows; host objects are platform-admin only.
- **Servers** (P5.1/P5.2): the control plane authorizes and audits, then
  forwards to the agent's `/orgs/<org>/...` over mTLS with its statement of
  the caller, and the agent authorizes again (org pinned twice; an agent
  refuses orgs not placed on it). Dedicated CA: agents get serverAuth-only
  certs, the control plane a clientAuth-only one, so no agent can call
  another. Server events are copied into the control plane's feed (only for
  orgs placed there). An org is placed once; the default org is local. The
  SSH key is used only for bootstrap. A remote org's secrets and history
  live on its server; `audit_list` stays on the control plane.
- **GitHub sign-in on titan:** the OAuth app "isb (titan)" (owner
  knowsuchagency, client id `Ov23liG4tb3bUknJiwrt`) has callbacks for
  `http://localhost:8192` (titan's isb.service) and `http://localhost:18990`
  (scratch tests). Its id and secret are in 1Password as
  `ISB_GITHUB_CLIENT_ID` / `ISB_GITHUB_CLIENT_SECRET`. A public hostname for
  isb needs its callback added to the app. Google sign-in is not set up.
- **Postgres reserves `pg_` role names:** a database app's default user is
  `app_<name>` when its name starts with `pg-`/`pg_` (it was the cause of
  "hyphenated databases never start").
- **CLI wart:** `isb db create` takes the app project as `--project`, which
  clashes with the global incus `--project`; `-P/--project-name` errors.
  Untangle when the CLI flags get a pass.
- **Bug: daemon and CLI disagree on the socket without XDG_RUNTIME_DIR.**
  `isb serve` as a system unit (root, no runtime dir) listens on
  `/tmp/isb-0/serve.sock`; the CLI over ssh (pam sets `/run/user/0`) looks
  in `/run/user/0/isb/`. Workaround on isb-test: `RuntimeDirectory=isb` +
  `ISB_SERVE_SOCKET=/run/isb/serve.sock`. Fix: one fallback order shared by
  both, and `isb serve install` for system units.
- **The default org is always a real org (`isb-default`)**, created by
  `isb serve` at start when missing (`org::ensure_default`); incus' own
  `default` project is never an org and holds only plain `isb create` /
  `isb up` sandboxes. Default-org stacks found with instances in incus'
  `default` project get a startup warning, nothing more (remove and
  redeploy them).
- **Service names need dnsmasq to reach the DNS root**: a root daemon (or
  agent) keeps state in `/var/lib/isb` (0700) around `/var/lib/isb/dns`, so
  names never resolved there; `isb serve` now makes those dirs 0711.
- **Shared types:** `isb::org::OrgId` (validated name, `incus_project()`,
  `dir(state)`) is the key every org-scoped module uses.
- **Remote servers: federation, not incus clustering.** Each server runs incus
  and `isb serve` as an agent; the control plane places orgs on servers and
  proxies to them over mTLS.
- **Local registry (P2.3) is TLS with an isb CA.** incus pulls OCI images
  only from `https` remotes (plain HTTP is refused) and does not import an
  OCI archive directly. It pulls through skopeo, which trusts a per-registry
  CA at `/etc/containers/certs.d/<host:port>/ca.crt`. So the registry runs
  with a cert from an isb-generated CA, and `isb host setup` installs that
  CA file. Pulls keep OCI semantics (`oci.entrypoint`, uid/gid). Verified
  on titan with `registry:2` and a pushed busybox.

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
- [x] (b9c6df4, a1f5cf5, 9b125e2) P0.2 `isb machine`: Lima-backed incus VM on macOS (init with CPU/memory/
  disk, start, stop, rm, status, ssh), incus installed from Zabbly, socket
  forwarded to `~/.isb/machine/<name>/incus.sock`, `$HOME` shared at the same
  path (so bind mounts work), default socket discovery uses it. Published
  ports reachable from the Mac's localhost. **Verify:** on minime, from
  nothing: `isb machine init && isb up` of the README example, port reachable
  from macOS; `isb tui` works.
- [x] (b9c6df4) P0.3 `isb serve` on macOS: launchd agent install (`isb serve install`
  writes a LaunchAgent plist), balancer listening on the Mac, reaching
  replicas in the VM. **Verify:** a 2-replica stack on minime, curl from
  macOS spreads across both.

## Phase 1: foundation

### Orgs
- [x] (orgs commits on platform) P1.1 Org model: `isb org create|ls|rm|show`, an org = incus project
  `isb-<org>` created restricted (no privileged, managed disks only,
  limits from the org's quota), per-org network (bridge) and default ACLs.
  Every command, tool and the daemon take `--org` (default: a `default`
  org mapped to the incus `default` project for backwards compatibility).
  Stacks and sandboxes live inside their org. **Verify:** two orgs, a stack
  in each, neither can see or reach the other (exec, list, network).
- [x] (fec1f95) P1.2 Per-org network policy: allow within the org, deny across orgs and
  to private ranges by default (done with P1.1: the org ACL), named exceptions
  in the org config (`--allow-egress CIDR[:PORTS[/proto]]`, done).
  **Verify:** curl across orgs fails, within succeeds, egress to internet ok.
- [x] (fec1f95) P1.3 Service discovery: stable names per service inside an org
  (`<service>.<stack>.isb` or similar), resolving to the instance (1 replica)
  or an org-local balancer address (replicas). **Verify:** an app reaches its
  postgres by name through a rolling replacement of the postgres.

### Identity and API
- [x] (0f5b645) P1.4 Users and sessions: built-in store (SQLite in the state dir),
  argon2id passwords, sessions with secure cookies, first-run admin setup,
  invitations, roles (platform admin; org admin/member), API tokens (hashed,
  org-scoped). **Verify:** unit tests + login over HTTP.
- [x] (97da457; GitHub sign-in verified end to end 2026-10-03; Google left out by Stephan's call) P1.5 OAuth/OIDC: GitHub, Google, generic OIDC (discovery, PKCE),
  account linking by verified email. **Verify:** GitHub login end to end in a
  browser against the hcloud box.
- [x] (97da457, 9f568c6: passkey register + sign-in in lasso's browser with a CDP virtual authenticator) P1.6 Passkeys (WebAuthn): register and sign in. **Verify:** browser on
  minime (Touch ID or a virtual authenticator via CDP).
- [x] (platform) P1.7 REST + SSE API generated from the tool registry: `/api/v1/<tool>`,
  OpenAPI document, `/api/v1/events` SSE, auth by session or token; MCP keeps
  working; per-org MCP endpoint `/orgs/<org>/mcp` bound to the caller's org.
  Cloudflare Access identities map to users. **Verify:** the same call via
  MCP, REST and CLI; an org token cannot touch another org.
- [x] (superadmin branch; tailnet and tokens verified live on titan, Access by unit tests with signed test JWTs, no live Cloudflare change) P1.9 Superadmins: the unix socket's
  reach (every tool, no remote-spec policy, any instance, the host tools)
  over HTTP from superadmin tokens (minted on the host only), tailnet
  identities (`--superadmin-tailnet`, whois from tailscaled) and verified
  Access identities (`--superadmin-access`); CSRF, Origin, Content-Type and
  Host checks for the ambient ones; audited by source; the web UI's Host
  page and badge. **Verify:** a listed tailnet identity is a superadmin and
  an unlisted one is not; a superadmin token makes a bind-mount sandbox a
  platform-admin token is refused; nobody mints a superadmin token over HTTP.

### Secrets
- [x] (a10c6fc, 1ee402b) P1.8 age store + driver trait + `isb secret create|set|get|ls|inspect|rm|
  encrypt|reencrypt|refresh`, per org; daemon key lookup and generation;
  break-glass recipients. **Verify:** unit tests; reencrypt round trip with a
  second recipient.
- [x] (fb3f72d) P1.9 Stacks reference secrets by name+version; revision uses versions;
  migrate existing stack state (base64 values) into the local store on daemon
  start. **Verify:** existing e2e stack survives the upgrade; a `secret set`
  rolls the dependent service.
- [x] (fb3f72d, acc77ad) P1.10 Inline `age:` secrets in compose, `{secret: name}` env delivery
  (unit env file; OCI incus config), external-driver refresh polling.
  **Verify:** integration test for each delivery path.
- [x] (platform) P1.11 `onepassword` driver (via `op` service account token stored as a
  local secret, or titan's broker). **Verify:** against a titan vault.
- [~] (e1ab878; Linux done, macOS keychain with P0.3) P1.12 Secret tools on MCP/REST (`secret_list|get|set|delete`), org-scoped;
  `isb serve install` creates the systemd credential (Linux) / keychain-backed
  file (macOS). **Verify:** an org agent rotates a secret over MCP and the
  service rolls.

## Phase 2: the deploy loop

- [~] (07e8dfc: generic git + webhooks done; GitHub App and the hcloud push check remain) P2.1 Git sources: GitHub App (install, repo list, webhooks), generic git
  over HTTPS/SSH with deploy keys, GitLab/Gitea webhooks. **Verify:** push to
  a test repo deploys on the hcloud box.
- [x] (5dbf468; buildpacks not supported: pack needs a docker daemon) P2.2 Builds in sandboxes: Railpack (and Nixpacks), Dockerfile,
  buildpacks; each build in a fresh isb sandbox (VM for untrusted), logs
  streamed, build cache volume per app. **Verify:** a Node, a Python and a
  Dockerfile app build and run.
- [x] (5dbf468) P2.3 Local OCI registry as an isb service; builds push, incus pulls;
  image retention. **Verify:** deploy pulls from the local registry; rollback
  to a previous image.
- [x] (0486011; Let's Encrypt cert verified on a cx23 in nbg1; live Cloudflare Tunnel check waits on Stephan's go-ahead) P2.4 Ingress: embedded edge proxy (Caddy) with ACME, `domains:` per
  service (host, path, port, https, redirects), generated hostnames
  (sslip.io-style) for quick starts; Cloudflare Tunnel as an alternative
  provider per org. **Verify:** HTTPS on the hcloud box with a real cert.
- [x] (07e8dfc) P2.5 App model + env editor: "application" (git/image source, build
  settings, env, domains, volumes, replicas) as a first-class object over
  stacks; project → environment → service hierarchy. **Verify:** create, edit,
  deploy and redeploy an app through the API.

## Phase 3: web UI

- [x] (9f568c6) P3.1 Scaffold: React + Vite + Tailwind + shadcn, bun build, embedded in
  the binary, served by `isb serve`; auth pages (sign in with SSO, GitHub,
  Google, email/password, passkey; create account; lost password).
  **Verify:** sign-in flows in the browser.
- [x] (3f0e441, 567dad2) P3.2 Dashboard + org/project/environment navigation; service pages with
  tabs: General, Environment, Domains, Deployments, Logs, Monitoring,
  Advanced; live updates over SSE. **Verify:** browser walkthrough recorded
  with screenshots.
- [x] (3f0e441, ed4ef6f; templates wait on P4.4) P3.3 Deploy flows: new app from git/image/template, deploy with live
  build logs, rollback, scale, web terminal (xterm.js over websocket exec).
  **Verify:** end to end in the browser.
- [x] (e7b84ec, d00c451) P3.4 Org admin: members, invitations, roles, API tokens, secrets editor,
  settings. **Verify:** invite a second user and sign in as them.
- [x] P3.5 MCP page (`/orgs/ORG/agents`, `G A`): the org endpoint, a token
  for the agent, install snippets (Claude Code, Codex, Cursor, curl), the
  tool list, and for superadmins the `/mcp` endpoint and its sources.
  **Verify:** the page's curl snippet lists the tools of a scratch daemon
  with a token made the page's way; screenshots as owner and superadmin.

## Phase 4: day 2

- [~] (a4c2bfd, ebb3f44: verified on titan with RustFS as the S3 store; MongoDB 8 cannot start on kernel ≥ 6.19; hcloud run pending) P4.1 Database templates (Postgres, MySQL/MariaDB, Redis, MongoDB) with
  credentials as secrets and scheduled backups to S3-compatible destinations;
  restore. **Verify:** backup and restore a Postgres on the hcloud box.
- [x] (a4c2bfd, ebb3f44) P4.2 Scheduled jobs (cron) per service/org. **Verify:** a job runs on
  schedule and its logs are visible.
- [x] (6d71ea9) P4.3 Notifications (Slack, Discord, Telegram, email, webhook) on deploy,
  failure, health, backup events. **Verify:** a webhook receives events.
- [x] (2aa485c, e153e1c, 0afef2b, 83fa46e: Uptime Kuma deployed from the web catalog) P4.4 Template catalog (one-click apps). **Stretch goal:** running Dokploy's
  templates (docker-compose + `template.toml`: variables, domains, mounts)
  directly; check the Dokploy/templates repo license before shipping its
  catalog. Only if it fits isb's architecture without bending it.
  **Verify:** deploy two native (and, if done, two Dokploy) templates from the UI.
- [x] (1502ebd; Gitea live; GitLab and fork previews unit-tested only) P4.5 Preview deployments per pull request. **Verify:** a PR on the test
  repo gets a URL; closing it removes it.
- [x] (6d71ea9, 83fa46e) P4.6 Metrics history (retained samples) and monitoring pages.
- [x] (template-logos) P4.7 Template logos: the daemon fetches and caches
  each template's logo and serves it from `/api/v1/templates/<ref>/logo`
  (SSRF-checked https, 512 KiB, sniffed image types); built-ins link to
  upstream logos. **Verify:** the Templates page shows built-in and Dokploy
  logos in light and dark mode, with no request leaving isb's origin.

## Phase 5: scale-out

- [x] (7ccde1f, 838011d; verified on a cx23 in nbg1, 13 min) P5.1 Remote servers: add a server (SSH bootstrap installs incus + isb
  agent), mTLS between control plane and agent, health. **Verify:** the
  hcloud box joins titan's (or a test) control plane.
- [x] (7ccde1f, 838011d) P5.2 Placement: orgs on servers; MCP/REST calls proxied to the owning
  server; secrets delivered only to servers that run their consumers.
  **Verify:** deploy to an org placed on the hcloud box from titan's UI.
- [x] (8423ecc, f48d234, 39d4158, cabc357, bf41199) P5.3 Audit log and finer roles. **Verify:** actions appear in the log
  with the acting user or agent.
- [x] (2cdfda8, 7e6bff1; wizard verified on titan against a throwaway VM, 1.5 min) P5.4 Placement in the web UI: a Servers page (health, resources, orgs,
  detail, remove) with an Add server wizard that follows the bootstrap
  (`server_add` with `wait: false`, `server_provision_get`); placement and
  isolation in the New org dialog, the org list and org Settings.
  **Verify:** add a throwaway box through the wizard; create orgs on each
  placement from the UI.
- [x] (2cdfda8, 7e6bff1; verified on titan: whoami in vm-plvm, VM from the UI in ~4 min, deleted with its org) P5.5 Dedicated VMs: `org_create` with `placement: {vm: {...}}`
  (`isb org create NAME --vm`) makes an incus VM in `isb-system`, installs
  incus and the control plane's own isb through the incus API, registers it
  as server `vm-NAME` and places the org there; `delete_vm` removes it with
  the org. Refused, with the reason, where the host has no KVM.
  **Verify:** an org in its own VM on titan runs an app, its server shows
  healthy, and deleting the org deletes the VM.

## Workspaces (docs/design/workspaces.md)

- [x] (workspaces-ssh) W3 Doors, generic over an org's instances: SSH over
  the daemon's websocket (`sshd -i` through incus exec, the caller's isb
  SSH keys per connection, live sessions re-checked every 15 s), `isb key`,
  the Account page's SSH keys, `isb ssh-proxy`, `isb ssh-config` (pinned
  host keys, herdr line), `ssh_host_keys`, `?instance=` on the web
  terminal, audit `ssh.open/close` and `auth.ssh_key_*` (docs/guides/ssh.md).
  **Verified** on titan: ssh, scp (20 MB both ways), a removed key refused
  and its live session ended in 8 s, viewer and `read` tokens refused,
  an instance's own authorized_keys ignored, `herdr machine add` saved and
  reached the host (isolated HOME).
- [x] (workspaces-home) W4 the home, generic over an org's named volumes:
  snapshots now and on a schedule (`auto-*` pruned to keep, manual kept),
  volume backups as `backup_*` with a `volume` (snapshot, temporary copy,
  incus export compressed and streamed to S3, retention, run logs, beside
  database backups), the `/etc/isb/pre-snapshot` hook (timeout, output in
  the run log, `hook_required`), staged restores into a new volume mounted
  at `/restore/<stamp>` (detached when the instance is stopped) and their
  discard, `volume_*` tools, `isb volume snapshot|restore|restores|discard`,
  the Volume panel and Volumes pages, admins-and-owners writes, org
  projects allowing snapshots and exports (docs/guides/volumes.md).
  **Verified** on titan (scratch daemon, org `volh`, a dev-base instance
  with a volume, RustFS as the S3 store): hook ran and its output logged; a
  per-minute schedule pruned to keep 2 with the manual snapshot kept; a
  5 s hook timeout reported and the snapshot taken, then refused with
  `hook_required`; write, snapshot, change, staged restore, `diff -r`
  showed the old file at `/restore/<stamp>` with ownership kept and the
  live volume untouched; backup to RustFS, retention to 2, restore from the
  bucket staged and diffed; restore while stopped left detached; discard
  refused a volume that was not a staged restore; the UI restored a backup
  file staged.
- [ ] W4 follow-ups: volume
  snapshots and backups for orgs placed on a server (tools forward, not
  verified); a restore's byte count in its run record.
- [x] (workspaces-core) W1 the workspace and its sandboxes, W2 the
  workspace as an org actor, W7 the web UI (docs/concepts/workspaces.md): one
  workspace per org (`max_workspaces`), a container with a home volume
  that survives rebuild, `workspace_*` tools, the `workspace` REST resource
  and `isb workspace`, confirmations that name live sessions; its `isb_ws_`
  token (role admin by default) delivered as /run/isb/token and $ISB_TOKEN
  with $ISB_URL and $ISB_ORG, rotated and revoked, actor `workspace`; the
  org-bound MCP on each org's bridge (port 8481, the org's subnet and bearer
  tokens only); sandbox expiry and idle timeout, `sandbox_extend`, the
  reaper; `isb workspace ssh`/`ssh-config`; the Workspace page, org
  overview card and create form. **Verified** on titan (scratch daemon,
  bridge port 8480 since titan's ufw has no 8481 rule): workspace from
  dev-base, token and env inside, tools/list and the isb CLI over the
  bridge, another org's path 404, no token 401, another org's instance
  cannot connect, a rotated token refused at once, rebuild keeps the home
  and drops the root, a sibling sandbox through the MCP labelled
  `isb.owner=workspace`, reaped on expiry (2m) and on idle (1m) with
  `sandbox.reaped` in the history, the audit actor `workspace`, the web
  terminal as `dev` counted as a live session, UI light/dark,
  desktop/phone. Homes: a volume's pool per host (`--workspace-pool`) and
  org (`home_pool`), hourly snapshots only on copy-on-write pools (titan's
  `dir` gets none, with the Home tab's warning), the Volume panel on the
  Home tab; host-folder homes (`--workspace-home-root`, `home_kind`,
  `home_bind` for superadmins), the folder allowed in the org's project and
  mapped 1:1. **Verified**: a host-folder home under the scratchpad (a file
  made inside is uid 1000 on the host, the path added to
  `restricted.devices.disk.paths`), and an org opting back to a volume on
  `dir` with no schedule.
- [x] (ws-fixes) Workspace rough edges: `workspace_*`/`sandbox_*` refuse an
  unknown org up front; incus project-limit refusals become "org X is at its
  CPU quota (limits.cpu 2, 2 in use)" with how to raise it, and a failed
  create removes what it made; host-folder homes are recorded on the project
  so `isb org create`/`org_update` keep them bindable; the image defaults to
  `dev-base` where it exists, else `images:ubuntu/24.04`, and the create
  form picks from the host's images and shows quota headroom. **Verified**
  on titan with a scratch `isb serve`, before and after.
- [ ] Workspace follow-ups: `isb host setup` on titan for port 8481 (not
  run: the rule is in the code); titan's `--workspace-home-root
  /srv/workspaces` and migrating clem with `home_bind`; a workspace on an org placed on a server
  (the agent runs it and serves the bridge; untested); W3's terminal
  reattach and ports; SSH to orgs placed on a server; Access credentials in
  `isb ssh-proxy`.

## Release 1.0

Finishing this workstream is isb **1.0.0** (Stephan, 2026-10-03), not another 0.x.

- [ ] Land the in-flight branches on `platform` (default org always `isb-default`, the EA theme and wordmark) and the docs pass that follows.
- [ ] Workspaces W5 (workspace images/templates) and W6 (the per-org Docker exception), or an explicit decision to ship 1.0 without them.
- [ ] Remote-server gaps: SSH and volume backups for orgs placed on a server; upgrading server agents and dedicated VMs.
- [ ] Full CI green on `platform`, integration tests on titan, a fresh-host install test on a new hcloud box (README quick start as written).
- [ ] PR `platform` → `main` with release notes (the user-visible changes since 0.7, and breaking changes: the default org, the crate split).
- [ ] Bump to 1.0.0 everywhere the release process lists, for all six crates together; tag; publish (crates.io `cargo publish --workspace`, PyPI, npm) per the release process.
- [ ] Upgrade titan's `isb.service` from 0.7.0; `isb host setup`; `--workspace-home-root /srv/workspaces`.
- [ ] Marketing site pulls isb docs from `main` (or the v1.0.0 tag) instead of `platform`.

## Log

- 2026-10-03: P1.1 done (orgs isolate, verified by integration test
  orgs_isolate); P1.4 identity and P1.8 secrets store merged. Notes from
  them: remote callers can use every secret tool across orgs until P1.7;
  reset tokens go to the journal when no mailer is set; a setup token in
  `<state>/setup-token` guards first-run setup.
- 2026-10-03: P1.1 core landed (org create/ls/show/rm, --org, host setup);
  stacks and the daemon are not org-aware yet (next).

- 2026-10-03: plan written; branch `platform` off main at 0.7.0.
