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

  `GET /api/v1/openapi.json` describes the REST surface, `GET /api/v1/tools`
  lists the tools, `/healthz` answers without auth, and the identity
  endpoints (sign-in, invitations, API tokens) are under `/api/v1/auth/*`
  ([auth.md](auth.md)). `POST /api/v1/webhooks/<org>/<app>` takes an app's
  push webhooks: no session, a signature or token instead, and served
  ahead of Access ([apps.md](apps.md#webhooks)).

`--listen` refuses anything but a loopback address: put a tunnel (or a
reverse proxy) in front of it, never an open port.

## Signing in, and what callers may reach

Every HTTP caller is an isb user. **The org is the trust boundary**: a member
of an org (any role) fully administers that org's stacks, sandboxes and
secrets, and nothing in any other org. Platform admins reach every org.

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
  `server_status` and re-encrypting every org's secrets are for platform
  admins.
- The unix socket is the daemon's own user and reaches everything.

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
| `stack_status` | One stack in detail: replicas, health, rotation, restarts, probe output, ports and backends. |
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
| `secret_create`, `secret_set`, `secret_get`, `secret_list`, `secret_inspect`, `secret_delete`, `secret_refresh`, `secret_reencrypt`, `secret_recipients`, `secret_resolve` | An org's secret store; values base64. `secret_set` and `secret_refresh` roll the stacks using the secret. `secret_resolve` (local callers only) is how `isb up` reads store-backed secrets. See [secrets.md](secrets.md). Remote callers reach every org's secrets, values included, unless `--deny-tools 'secret_*'`. |
| `project_create`, `project_list`, `project_delete`, `environment_create`, `environment_list`, `environment_delete` | Projects and their environments; each environment runs its apps as the stack `<project>-<env>`. See [apps.md](apps.md). |
| `app_create`, `app_get`, `app_list`, `app_update`, `app_delete` | Apps: an image or a repository with a builder, plus env, domains, volumes, ports, replicas, port, health check, resources, command. |
| `app_deploy`, `app_rollback`, `app_deployments`, `app_deployment_log` | Deployments: queue one (`wait` blocks), go back to an earlier one's image and settings, the history, a deployment's log from an offset. |
| `app_env_get`, `app_env_set`, `app_webhook`, `app_deploy_key` | An app's environment as `.env` text; its webhook path and secret (`rotate`); a new SSH deploy key. |
| `server_status` | Versions, and the balancer's routes with live counters. |
| `overview` | Everything a dashboard shows in one call: host CPU and memory with history, every stack in detail, sandboxes with their CPU and memory, the latest event number. |
| `events` | The event feed (deploys, rollouts, health changes, restarts, failures) after a `since` cursor, optionally waiting up to 30 s for one. |

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
