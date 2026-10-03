# `isb serve`: the daemon and its MCP server

`isb serve` runs the stack controller ([stacks.md](stacks.md)) and the app
layer over it ([apps.md](apps.md)), and exposes them, together with sandbox
management, as [Model Context Protocol](https://modelcontextprotocol.io)
tools. It listens in two places:

- a **unix socket** (`$ISB_SERVE_SOCKET`, else `$XDG_RUNTIME_DIR/isb/serve.sock`;
  0600 in a 0700 directory) for the local `isb stack` CLI. Its callers are the
  daemon's own user and are trusted.
- **loopback HTTP** (`--listen`, e.g. `127.0.0.1:8092`) for remote people and
  agents, reached through a tunnel or reverse proxy. It serves the same tools
  four ways:

  | Path | What |
  |---|---|
  | `/mcp` | MCP (Streamable HTTP); every tool takes an `org` argument |
  | `/orgs/<org>/mcp` | MCP bound to one org: `org` is filled in, and any other value is refused |
  | `POST /api/v1/tools/<tool>`, `/orgs/<org>/api/v1/tools/<tool>` | REST: the arguments as a JSON body; `{"result": ...}`, or `{"error", "message", "data"}` with a matching status (400, 401, 403, 404, 409, 500, 504) |
  | `GET /api/v1/events` | server-sent events: deploys, rollouts, health and restarts in the caller's orgs; resumes from `Last-Event-ID` or `?since=` |
  | `GET /orgs/<org>/api/v1/terminal?app=NAME` | a websocket to a shell in one of the app's replicas ([below](#the-web-terminal)) |

  `GET /api/v1/openapi.json` describes the REST surface, `GET /api/v1/tools`
  lists the tools, `/healthz` answers without auth, and the identity
  endpoints (sign-in, invitations, API tokens) are under `/api/v1/auth/*`
  ([auth.md](auth.md)). `POST /api/v1/webhooks/<org>/<app>` takes an app's
  push webhooks: no session, a signature or token instead, and served
  ahead of Access ([apps.md](apps.md#webhooks)). Every other `GET` is the
  **web UI**, embedded in the binary: sign-in, invitations, accounts and a
  live dashboard, built on the same API ([web.md](web.md)). It never answers
  an API path.

`--listen` refuses anything but a loopback address: put a tunnel (or a
reverse proxy) in front of it, never an open port.

## Signing in, and what callers may reach

Every HTTP caller is an isb user. **The org is the trust boundary**: a member
of an org (member, admin or owner) fully administers that org's stacks,
sandboxes and secrets, and nothing in any other org; a viewer only reads it
(read-only tools, no secret values, no exec). Platform admins reach every
org. API tokens can be narrowed with scopes (`read`, `deploy`, `admin`,
`tool:GLOB`; [auth.md](auth.md#scopes)), judged in the same authorizer.

- **API tokens** (`Authorization: Bearer isb_tok_...`, from `isb token create`
  or the web UI) are how agents sign in. A token made for an org reaches only
  that org, which is what an agent running inside the org should hold:
  `isb token create agent --org ocai`, then point it at `/orgs/ocai/mcp`.
- **Sessions** (the `isb_session` cookie from `POST /api/v1/auth/login`) are
  how the web UI signs in. Cookie-authenticated writes must carry
  `X-Isb-Csrf: 1`.
- **Cloudflare Access** (optional; below) puts SSO in front of everything. An
  Access identity whose email belongs to an isb user acts as that user; one
  that does not gets "ask an org admin to invite you".
- **Anonymous** calls are refused, unless `--allow-unauthenticated` (local
  testing only).
- `overview`, `events` and `stack_list` show only the caller's orgs.
  `server_status`, `org_list`, `org_create`, `org_update`, `org_delete` and
  re-encrypting every org's secrets are for platform admins, whatever their
  role in an org (an org owner's token is refused). `org_get` is for the
  org's members.
- `audit_list` shows an org's owners and admins their org's entries and
  platform admins everything; `audit_verify` is for platform admins.
- The unix socket is the daemon's own user and reaches everything.
- Every call that changes something, every refusal, every secret read,
  sign-in, webhook delivery and terminal session is recorded in the audit
  log ([audit.md](audit.md)).

## The web terminal

`GET /orgs/<org>/api/v1/terminal?app=NAME[&slot=N][&cols=C&rows=R]`
upgrades to a websocket bridged to a login shell (bash, else sh) in one of
the app's running replicas (`slot`, or one in rotation), with a
pseudo-terminal. The web UI's Terminal tab uses it.

- **Who**: the caller signs in as for any tool (session, API token, Access)
  and is admitted as if calling `sandbox_exec` in the org: the org's
  members, admins and owners (not viewers, nor tokens whose scopes leave
  out `sandbox_exec`), platform admins, and nobody when `--deny-tools`
  covers `sandbox_exec`. Only replicas of the org's own apps are reachable.
- **Cross-site**: a session cookie rides along on a websocket from any
  site, so a cookie-authenticated upgrade must carry an `Origin` naming the
  request's `Host`. A bearer token needs no `Origin` (a browser never adds
  one by itself).
- **Wire**: binary frames are terminal bytes both ways. Text frames are
  JSON: `{"type": "resize", "cols", "rows"}` from the client;
  `{"type": "exit", "code"}` and `{"type": "error", "message"}` from the
  server before it closes. What fails after the upgrade (no such app, a
  replica that is not running, an image without a shell) arrives as an
  error frame.
- **Bounds**: 16 terminals at once per daemon, closed after 30 minutes
  without a byte either way and after 8 hours in all, messages up to
  64 KiB. Closing the websocket kills the shell. Each opening is an event on
  the app's service, with the caller, and the audit log records the opening
  (or its refusal) and the closing with how long it ran, never the
  keystrokes.

## Install

```sh
isb serve install          # writes ~/.config/systemd/user/isb.service, enables and starts it
systemctl --user status isb
journalctl --user -u isb -f
```

The unit runs `isb serve` with `~/.config/isb/serve.env` as its environment,
restarting it always. The installer writes that file with defaults only if it is
missing, then waits for `/healthz`. It is idempotent: run it again after
changing the env file or upgrading isb. If the binary path contains a version
(a mise install), the unit pins that version; rerun `isb serve install` after an
upgrade. Without lingering (`loginctl enable-linger $USER`), user services stop
when you log out; the installer says so.

The installer also sets up the daemon's secrets key. Where `systemd-creds` can
make user credentials (systemd 256 or later), it encrypts the key (generating
one if there is none) into `~/.config/isb/isb-age-key.cred` and the unit loads
it with `LoadCredentialEncrypted=isb-age-key:%h/.config/isb/isb-age-key.cred`;
it then prints the command that removes the plaintext key, and why to add a
break-glass recipient first. On an older systemd the daemon keeps reading
`~/.config/isb/age.txt`, and the installer says to keep that file out of
backups. See [secrets.md](secrets.md#the-daemons-key).

On macOS the daemon runs inside the `isb machine` VM, and `isb serve install`
writes a LaunchAgent that starts the machine at login instead; see
[macos.md](macos.md#the-daemon-lives-in-the-vm).

The daemon's user needs the incus socket (usually the `incus-admin` group).
Stack definitions are kept in `$XDG_STATE_HOME/isb/stacks/` (0600 files;
they hold references to secrets, never values), override with `--state-dir`.
The secret store is under `<state-dir>/orgs/`, encrypted to the daemon's age
key, which it finds (or generates) at startup; see
[secrets.md](secrets.md#the-daemons-key).

## Remote MCP through Cloudflare Tunnel and Access

The pattern is the one [herdr-mcp](https://github.com/Orange-County-AI/herdr-mcp)
uses: Cloudflare runs the OAuth flow, isb stays the resource origin and checks
the identity Cloudflare signs.

### 1. Serve on loopback

```dotenv
# ~/.config/isb/serve.env
ISB_SERVE_LISTEN=127.0.0.1:8092
CF_ACCESS_TEAM_DOMAIN=https://your-team.cloudflareaccess.com
CF_ACCESS_AUD=your-access-application-audience-tag
```

`systemctl --user restart isb`. Until both `CF_ACCESS_*` values are set, the
TCP listener serves `/healthz` and no tools.

### 2. Route the tunnel

Add an ingress rule to the named tunnel's configuration, then validate and
restart cloudflared:

```yaml
- hostname: isb.example.com
  service: http://127.0.0.1:8092
```

### 3. Protect it with Access Managed OAuth

In Zero Trust, under Access controls, Applications:

1. create an **MCP server** application for the hostname;
2. add an allow policy for the people (or service tokens) who may use it;
3. enable **Managed OAuth** in its advanced settings, allowing the redirect
   URIs of the clients you use (Claude, ChatGPT);
4. copy its **Application Audience (AUD) tag** into `CF_ACCESS_AUD`.

With both values set, every `/mcp` request must carry `Cf-Access-Jwt-Assertion`.
isb fetches the team's signing keys from `<team>/cdn-cgi/access/certs` (cached
for an hour, refetched for an unknown key id, rate-limited), and verifies the
RS256 signature, the issuer, the audience and the expiry. A request that reaches
the port by any other route is refused. Do not put another OAuth server behind
Access Managed OAuth: Access replaces the origin's 401 handling by design.

### 4. Connect a client

The MCP URL is `https://isb.example.com/mcp`. Claude adds it as a remote
(Streamable HTTP) MCP server; ChatGPT as a custom connector. The first
connection opens the Access login; no secret is copied into either product.

Every call is logged to the daemon's stderr (the journal) with the caller's
identity, the tool, its duration and whether it failed, never its arguments.
Stacks record who deployed them (`deployed_by`).

## What a remote caller's specs may ask for

The incus socket is root on the host. A remote caller is trusted to run
workloads, not to own the host. An org other than `default` is a restricted
incus project, so incus itself refuses most of what follows
([orgs.md](orgs.md)); the `default` org is incus' default project and is
held to these checks by isb. Refused unless the operator allows it:

| Refused | Allowed by |
|---|---|
| `privileged: true` | `--allow-privileged` |
| `raw_config`, `raw_devices`, `incus_profiles`, an `idmap` other than `auto`/`none`, guest-bound ports (`bind: guest`, a guest reaching into the host) | `--allow-raw` |
| bind mounts, and any whose real path (symlinks followed) is outside the roots | `--bind-root DIR` (repeatable) |
| publishing a port on anything but loopback | `--publish-address IP` (repeatable), e.g. a tailnet address |
| reaching instances `isb serve` does not manage (exec, remove, list) | `--any-instance` |
| unix-socket listeners on the host, a different `incus_project`, `isb.*` labels, secrets read from host files | never |

And always:

- `${VAR}` in a remote caller's compose file is filled from the `vars` it sent,
  never from the daemon's environment.
- `file:` and `environment:` secret values come in the call (`secrets`, or
  `vars` for an `environment:` secret) and are stored in the org's store;
  `external`, `age` and `driver` secrets are read by the daemon.
- Relative paths resolve in `<state-dir>/files/<stack>` unless the caller
  names a `base_dir` inside a bind root.
- A sandbox a remote caller creates is labelled `isb.owner=mcp:<identity>`.

`--allow-tools` and `--deny-tools` (names or globs such as `sandbox_*`,
comma-separated; deny wins) choose which tools remote callers see at all. The
local socket always has every tool and no policy: its caller could run `isb`
directly.

## Tools

| Tool | Does |
|---|---|
| `stack_deploy` | Deploy or update a stack from compose YAML (`name`, `compose`, `vars`, `secrets`, `base_dir`, `wait`, `timeout`). Returns the change per service; `wait` blocks until it settles. |
| `stack_list` | Every stack with its services' state. |
| `stack_status` | One stack in detail: replicas, health, rotation, restarts, probe output, ports and backends, and each domain with its URL and certificate state. |
| `stack_config` | The deployed compose file, and its secrets as references (store name, driver, version), never values. |
| `stack_logs` | Recent output of a service's replicas. |
| `stack_scale` | Set a service's replicas. |
| `stack_redeploy` | Replace a service's replicas though nothing changed. |
| `stack_rollback` | Back to the previous deployment. |
| `stack_remove` | Delete a stack's instances and ports (volumes with `volumes: true`), and the `<stack>_<key>` secrets it stored that no other stack uses. |
| `sandbox_create` | Create or reconcile one sandbox from a service spec (object or YAML). |
| `sandbox_list` | Instances, filtered by labels. |
| `sandbox_exec` | Run argv in a sandbox: exit code, stdout, stderr (each capped at 256 KiB, keeping the end), optional stdin text and timeout (default 10m). |
| `sandbox_remove` | Delete a sandbox (not a stack replica). |
| `secret_create`, `secret_set`, `secret_get`, `secret_list`, `secret_inspect`, `secret_delete`, `secret_refresh`, `secret_reencrypt`, `secret_recipients`, `secret_resolve` | An org's secret store; values base64. `secret_list` says which stacks use each secret. `secret_set` and `secret_refresh` roll the stacks using the secret. `secret_resolve` (local callers only) is how `isb up` reads store-backed secrets. See [secrets.md](secrets.md). Remote callers reach every org's secrets, values included, unless `--deny-tools 'secret_*'`. |
| `project_create`, `project_list`, `project_delete`, `environment_create`, `environment_list`, `environment_delete` | Projects and their environments; each environment runs its apps as the stack `<project>-<env>`. See [apps.md](apps.md). |
| `app_create`, `app_get`, `app_list`, `app_update`, `app_delete` | Apps: an image or a repository with a builder, plus env, domains, volumes, files, ports, replicas, port, health check, resources, command, user, working directory. |
| `app_deploy`, `app_rollback`, `app_deployments`, `app_deployment_log` | Deployments: queue one (`wait` blocks), go back to an earlier one's image and settings, the history, a deployment's log from an offset. |
| `app_env_get`, `app_env_set`, `app_webhook`, `app_deploy_key` | An app's environment as `.env` text; its webhook path and secret (`rotate`); a new SSH deploy key. |
| `preview_list`, `preview_get`, `preview_log`, `preview_redeploy`, `preview_delete` | Preview deployments per pull request (settings: the app's `previews`, through `app_update`): list (one app or all), one with its deployments, a deployment's log from an offset, build the head again, remove one now. See [previews.md](previews.md). |
| `template_list`, `template_get` | One-click apps: the built-in catalog and added ones (search by words, tag, catalog); a template's variables, apps, notes and, for a Dokploy template, its translation report. See [templates.md](templates.md). |
| `template_deploy` | Deploy a template into a project environment as apps (`template`, `project`, `environment`, `name`, `values`, `dry_run`, `wait`, `timeout`): generated secrets go to the org's store, the apps deploy in dependency order. `dry_run` returns the plan. |
| `template_instance_list`, `template_instance_delete` | Deployed templates; deleting one removes its apps (named volumes are kept) and the secrets it made. |
| `template_catalog_list`, `template_catalog_add`, `template_catalog_remove` | The catalogs added to the built-in one: a host directory or an https URL, isb's format or Dokploy's. Adding and removing: platform admins. |
| `database_create`, `database_list`, `database_get` | Databases (Postgres, MySQL, MariaDB, MongoDB, Redis) as apps with a `database` source: credentials generated as org secrets, connection details with the password as a secret reference (`database_get` with `reveal` shows it). Deploy, update and delete them with the `app_*` tools. See [databases.md](databases.md). |
| `backup_destination_create`, `backup_destination_list`, `backup_destination_delete`, `backup_destination_test` | S3-compatible buckets for backups; key pairs kept as org secrets. Loopback endpoints: local callers and platform admins only. |
| `backup_create`, `backup_update`, `backup_list`, `backup_delete`, `backup_run`, `backup_runs`, `backup_run_log`, `backup_restore` | Scheduled database backups (cron, keep N, gzip or zstd), their files in the bucket (`backup_list` with `name`), runs and logs; restore into an existing database (`confirm: true`) or a new one. |
| `job_create`, `job_list`, `job_get`, `job_update`, `job_delete`, `job_run`, `job_runs`, `job_run_log` | Scheduled jobs: a command on a cron schedule in an app's replica or a one-off instance from its image; runs with exit code, duration and output. See [jobs.md](jobs.md). |
| `build_run` | Start a build of a host directory (`app`, `context`, `builder`, `dockerfile`, `target`, `args`, `tag`, `untrusted`, `timeout`) into the org's registry repository; returns an id. Remote callers: `context` under a `--bind-root`. See [builds.md](builds.md). |
| `build_logs` | A build's state and log lines from `since`, waiting up to 30 s for more; `image` and `digest` when it succeeded. |
| `build_list` | The org's recent builds. |
| `registry_list` | The org's images in the local registry: apps, tags, digests, push times. |
| `registry_gc` | Registry retention (platform admins): keep the newest `keep` tags per app and whatever deployed stacks run or would roll back to. |
| `server_status` | Versions, and the balancer's routes with live counters. |
| `ingress_status` | The ingress: listeners, CA, the Caddy process, every routed domain (URL, certificate state, upstreams), conflicts and refusals, and each tunnel org's cloudflared ([ingress.md](ingress.md)). Shows the caller's orgs. |
| `org_get` | One org: limits and per-instance defaults, bridge and subnet, egress exceptions, bind roots, service-name domain, and instance, stack and member counts. Members of the org. |
| `org_list` | Every org, as `org_get` shows one. Platform admins. |
| `org_create` | Create an org (`org`, optional `cpus`, `memory`, `disk`, `instances`, `default_cpus`, `default_memory`, `egress`); fails if it exists. Platform admins. |
| `org_update` | Change an org's limits, defaults or `egress` (which replaces the list; `[]` clears it). Fields left out are kept. Platform admins. |
| `org_delete` | Delete an org and its members, invitations and tokens; refused while stacks are deployed in it, and while it has sandboxes unless `force`. Platform admins. See [orgs.md](orgs.md). |
| `notification_channel_create`, `notification_channel_list`, `notification_channel_get`, `notification_channel_update`, `notification_channel_delete`, `notification_test`, `notification_deliveries` | An org's notification channels (webhook, Slack, Discord, Telegram, email; URLs and tokens as org secrets), their rules on event kinds, a test send, and each channel's delivery log. See [notifications.md](notifications.md). |
| `notification_settings` | Whether channels may reach loopback and private addresses (off by default). Platform admins. |
| `metrics_query` | Metrics history of an org's instances (CPU, memory, network, disk I/O; 30 days in tiers): per instance, or summed/averaged over a service's replicas. See [metrics.md](metrics.md). |
| `overview` | Everything a dashboard shows in one call: host CPU and memory with history, every stack in detail, sandboxes with their CPU and memory, the latest event number. |
| `events` | The event feed (deploys, rollouts, health changes, restarts, failures) after a `since` cursor, optionally waiting up to 30 s for one. |
| `audit_list` | The audit log, filtered (actor, action and target globs, outcome, surface, time) and paged; an org's owners and admins see their org, platform admins everything ([audit.md](audit.md)). |
| `audit_verify` | Walk the audit log's hash chain. Platform admins. |

`stack_deploy` also takes `dry_run: true`, which returns the per-service
changes without deploying. `isb tui` ([tui.md](tui.md)) is built on
`overview`, `events` and `dry_run`; a web UI can be too.

The server speaks MCP's Streamable HTTP transport with plain JSON responses
(no SSE stream; `GET /mcp` is 405), statelessly: `tools/call` works without a
prior `initialize`, and there is no session id.

## Flags

| Flag | Environment | Default |
|---|---|---|
| `--listen` | `ISB_SERVE_LISTEN` | none: the socket only |
| `--serve-socket` | `ISB_SERVE_SOCKET` | `$XDG_RUNTIME_DIR/isb/serve.sock` |
| `--state-dir` | `ISB_SERVE_STATE_DIR` | `$XDG_STATE_HOME/isb` |
| `--interval` | | `5s`: how often each service is reconciled |
| `--access-team-domain` | `CF_ACCESS_TEAM_DOMAIN` | |
| `--access-aud` | `CF_ACCESS_AUD` | |
| `--allow-unauthenticated` | `ISB_SERVE_ALLOW_UNAUTHENTICATED` | off: remote MCP with no Access, for local testing only |
| `--allow-tools`, `--deny-tools` | `ISB_SERVE_ALLOW_TOOLS`, `ISB_SERVE_DENY_TOOLS` | all tools |
| `--bind-root` | `ISB_SERVE_BIND_ROOTS` (comma-separated) | none |
| `--publish-address` | `ISB_SERVE_PUBLISH_ADDRESSES` (comma-separated) | none: loopback only |
| `--allow-privileged`, `--allow-raw`, `--any-instance` | `ISB_SERVE_ALLOW_PRIVILEGED`, `ISB_SERVE_ALLOW_RAW`, `ISB_SERVE_ANY_INSTANCE` | off |
| `--public-url` | `ISB_PUBLIC_URL` | none: invitation and reset links are bare tokens, and provider sign-in and passkeys are off |
| `--session-max-age` | `ISB_SESSION_MAX_AGE` | `30d` |
| `--session-idle` | `ISB_SESSION_IDLE` | `7d` |
| `--github-client-id`, `--google-client-id` | `ISB_GITHUB_CLIENT_ID`, `ISB_GOOGLE_CLIENT_ID` | off; secrets in `ISB_GITHUB_CLIENT_SECRET`, `ISB_GOOGLE_CLIENT_SECRET` or the default org's secrets ([auth.md](auth.md#signing-in-with-github-google-or-oidc)) |
| `--oidc-issuer`, `--oidc-client-id`, `--oidc-name` | `ISB_OIDC_ISSUER`, `ISB_OIDC_CLIENT_ID`, `ISB_OIDC_NAME` | off; secret in `ISB_OIDC_CLIENT_SECRET` or the default org's secrets |
| `--open-signup` | `ISB_OPEN_SIGNUP` | off: provider sign-up needs an invitation |
| `--ingress-http`, `--ingress-https` | `ISB_INGRESS_HTTP`, `ISB_INGRESS_HTTPS` | off; `IP:PORT` (`:80` is every address). Either turns the ingress on ([ingress.md](ingress.md)) |
| `--ingress-tunnels` | `ISB_INGRESS_TUNNELS` | off: turns the ingress on for Cloudflare-tunnel orgs without public listeners |
| `--ingress-tunnel-port` | `ISB_INGRESS_TUNNEL_PORT` | `8480`: tunnel orgs' listener port on their bridge address |
| `--ingress-public-ip` | `ISB_INGRESS_PUBLIC_IP` | the default route's source address, if public: what `host: auto` names resolve to |
| `--acme-ca` | `ISB_ACME_CA` | `letsencrypt`; or `letsencrypt-staging`, `internal`, an ACME directory URL |
| `--acme-email` | `ISB_ACME_EMAIL` | none: the ACME account's contact |
| `--caddy-bin` | `ISB_CADDY_BIN` | the pinned Caddy release, downloaded and checked |
| `--audit-retention` | `ISB_AUDIT_RETENTION` | `90d`: how long audit entries are kept ([audit.md](audit.md)) |
| `--audit-all` | `ISB_AUDIT_ALL` | off: read-only tool calls are not recorded (secret reads and refusals always are) |
