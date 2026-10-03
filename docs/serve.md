# `isb serve`: the daemon and its MCP server

`isb serve` runs the stack controller ([stacks.md](stacks.md)) and exposes it,
together with sandbox management, as [Model Context Protocol](https://modelcontextprotocol.io)
tools. It listens in two places:

- a **unix socket** (`$ISB_SERVE_SOCKET`, else `$XDG_RUNTIME_DIR/isb/serve.sock`;
  0600 in a 0700 directory) for the local `isb stack` CLI. Its callers are the
  daemon's own user and are trusted.
- **loopback HTTP** (`--listen`, e.g. `127.0.0.1:8092`) at `/mcp` for remote
  agents such as Claude or ChatGPT, reached through a Cloudflare Tunnel and
  protected by Cloudflare Access. `/healthz` answers there without auth, and
  the identity endpoints (users, sessions, invitations, API tokens) answer at
  `/api/v1/auth/*`; see [auth.md](auth.md).

`--listen` refuses anything but a loopback address: remote access belongs
behind the tunnel and an Access policy, never on an open port.

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

## What a remote caller may do

The incus socket is root on the host. A remote caller is trusted to run
workloads, not to own the host, so its requests are checked first. Refused
unless the operator allows it:

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
  Remote callers can reach only those and stack instances; any other instance
  answers "not found".

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
| `--public-url` | `ISB_PUBLIC_URL` | none: invitation and reset links are bare tokens |
| `--session-max-age` | `ISB_SESSION_MAX_AGE` | `30d` |
| `--session-idle` | `ISB_SESSION_IDLE` | `7d` |
