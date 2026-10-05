---
title: Configuration
description: Every isb serve flag with its environment variable and default, every other variable isb reads, and the files it keeps its settings in.
order: 6
---

isb has no configuration file of its own beyond the daemon's environment
file: the `isb serve` daemon is configured with flags or the matching
environment variables, and everything else isb reads is listed below.
`isb serve install` runs the daemon with `~/.config/isb/serve.env` as its
environment, so the usual way to change a setting is a line in that file
and `systemctl --user restart isb`.

```dotenv
# ~/.config/isb/serve.env
ISB_SERVE_LISTEN=127.0.0.1:8092
ISB_PUBLIC_URL=https://isb.example.com
CF_ACCESS_TEAM_DOMAIN=https://your-team.cloudflareaccess.com
CF_ACCESS_AUD=your-access-application-audience-tag
```

## Daemon flags

Flags of `isb serve`. A flag given on the command line wins over its
environment variable. Durations take `ms`, `s`, `m`, `h` and `d`. On/off
flags (and their variables) take `1`/`0`, `true`/`false`, `yes`/`no`, `on`/`off`,
in any case; the bare flag means true, and a value on the command line is
written `--flag=false`.

### Listeners and the remote policy

| Flag | Environment | Default | |
|---|---|---|---|
| `--listen` | `ISB_SERVE_LISTEN` | none: the unix socket only. `isb serve install` writes `127.0.0.1:8092` into `serve.env` | HTTP listeners, comma-separated: loopback addresses, and tailnet ones (100.64.0.0/10, fd7a:115c:a1e0::/48); anything else is refused |
| `--serve-socket` | `ISB_SERVE_SOCKET` | `$XDG_RUNTIME_DIR/isb/serve.sock`, else `<tmp>/isb-<uid>/serve.sock` | the unix socket for the local CLI |
| `--state-dir` | `ISB_SERVE_STATE_DIR` | `$XDG_STATE_HOME/isb`, else `~/.local/state/isb` | everything the daemon keeps ([The state directory](../operations/host-setup.md#the-state-directory)) |
| `--interval` | | `5s` | how often each service is reconciled and health-checked |
| `--access-team-domain` | `CF_ACCESS_TEAM_DOMAIN` | | Cloudflare Access team domain (`https://TEAM.cloudflareaccess.com`) |
| `--access-aud` | `CF_ACCESS_AUD` | | the Access application's audience (AUD) tag |
| `--allow-unauthenticated` | `ISB_SERVE_ALLOW_UNAUTHENTICATED` | off | serve anonymous callers with no Access; local testing only |
| `--allow-tools`, `--deny-tools` | `ISB_SERVE_ALLOW_TOOLS`, `ISB_SERVE_DENY_TOOLS` | all tools | which tools remote callers see: names or globs (`sandbox_*`), comma-separated; deny wins |
| `--bind-root` | `ISB_SERVE_BIND_ROOTS` (comma-separated) | none | host directories remote callers may bind-mount from |
| `--publish-address` | `ISB_SERVE_PUBLISH_ADDRESSES` (comma-separated) | none: loopback only | host addresses remote callers may publish ports on |
| `--allow-privileged` | `ISB_SERVE_ALLOW_PRIVILEGED` | off | let remote callers create privileged containers |
| `--allow-raw` | `ISB_SERVE_ALLOW_RAW` | off | let remote callers use `raw_config`, `raw_devices` (other than `root: {size}`), `incus_profiles`, `idmap` maps and guest-bound ports |
| `--any-instance` | `ISB_SERVE_ANY_INSTANCE` | off | let remote callers reach instances isb does not manage |

With HTTP listeners and Access configured, Access guards the loopback
listeners. Without Access, the listeners serve callers that sign in with an
API token or a session (and tailnet superadmins); anonymous callers are
refused unless `--allow-unauthenticated`. What the remote policy flags
allow is explained in [The remote-spec
policy](../concepts/security.md#the-remote-spec-policy).

### Superadmins

| Flag | Environment | Default | |
|---|---|---|---|
| `--superadmin-tailnet` | `ISB_SUPERADMIN_TAILNET` | off | tailnet login names and `tag:` node tags, comma-separated, that are superadmins on a tailnet `--listen` address (orgs map their own tailnet agents, [Agent identities](../concepts/access.md#agent-identities)) |
| `--superadmin-access` | `ISB_SUPERADMIN_ACCESS` | off | Access emails and service token client ids, comma-separated and exact, that are superadmins; needs Access and `--public-url` |

Both are read at start-up, as the bootstrap. Identities added with `isb
superadmin add` are kept in `isb.db` and count beside them, with no flag and
no restart ([Superadmin identities in
isb.db](../concepts/access.md#superadmin-identities-in-isbdb)). See
[Superadmins](../concepts/access.md#superadmins) and
[Reach isb serve remotely](../guides/remote-access.md).

### Identity

| Flag | Environment | Default | |
|---|---|---|---|
| `--public-url` | `ISB_PUBLIC_URL` | none: invitation and reset links are bare tokens, and provider sign-in and passkeys are off | where users reach isb (`https://isb.example.com`) |
| `--session-max-age` | `ISB_SESSION_MAX_AGE` | `30d` | a browser session ends this long after sign-in |
| `--session-idle` | `ISB_SESSION_IDLE` | `7d` | a browser session ends after this long unused |
| `--github-client-id` | `ISB_GITHUB_CLIENT_ID` | off | GitHub OAuth app client id |
| `--github-url`, `--github-api-url` | `ISB_GITHUB_URL`, `ISB_GITHUB_API_URL` | `https://github.com`, `https://api.github.com` | GitHub Enterprise Server's web and API URLs (hidden from `--help`) |
| `--google-client-id` | `ISB_GOOGLE_CLIENT_ID` | off | Google OAuth client id |
| `--oidc-issuer`, `--oidc-client-id` | `ISB_OIDC_ISSUER`, `ISB_OIDC_CLIENT_ID` | off | a generic OpenID Connect provider |
| `--oidc-name` | `ISB_OIDC_NAME` | `SSO` | the generic provider's button label |
| `--open-signup` | `ISB_OPEN_SIGNUP` | off: provider sign-up needs an invitation | let a verified provider email make an account |

### Heartbeat

| Flag | Environment | Default | |
|---|---|---|---|
| `--heartbeat-url` | `ISB_HEARTBEAT_URL` | off | GET this URL every interval, a dead man's switch for an outside check (healthchecks.io and the like); its path is usually a token, so prefer the variable ([Uptime monitoring](../guides/uptime.md#host-down-a-dead-mans-switch)) |
| `--heartbeat-interval` | `ISB_HEARTBEAT_INTERVAL` | `60s` | how often, 10 s to 1 h |

Client secrets are never flags (argv shows in `ps`): they come from
`ISB_GITHUB_CLIENT_SECRET`, `ISB_GOOGLE_CLIENT_SECRET` and
`ISB_OIDC_CLIENT_SECRET`, or, when unset, a secret of the same name in the
`default` org, read at each sign-in. See [Sign-in](../guides/sign-in.md).

### Ingress

| Flag | Environment | Default | |
|---|---|---|---|
| `--ingress-http` | `ISB_INGRESS_HTTP` | off | `IP:PORT` for plain HTTP (`:80` is every address); turns the ingress on |
| `--ingress-https` | `ISB_INGRESS_HTTPS` | off | `IP:PORT` for HTTPS; turns the ingress on |
| `--ingress-tunnels` | `ISB_INGRESS_TUNNELS` | off | turn the ingress on for Cloudflare-tunnel orgs without public listeners |
| `--ingress-tunnel-port` | `ISB_INGRESS_TUNNEL_PORT` | `8480` | tunnel orgs' listener port on their bridge address |
| `--ingress-public-ip` | `ISB_INGRESS_PUBLIC_IP` | the default route's source address, if public | what `host: auto` names resolve to |
| `--acme-ca` | `ISB_ACME_CA` | `letsencrypt` | or `letsencrypt-staging`, `internal`, an ACME directory URL |
| `--acme-email` | `ISB_ACME_EMAIL` | none | the ACME account's contact |
| `--caddy-bin` | `ISB_CADDY_BIN` | the pinned Caddy release, downloaded and checked | a Caddy binary to run instead |

See [Domains and ingress](../guides/domains.md).

### Workspaces

| Flag | Environment | Default | |
|---|---|---|---|
| `--workspace-mcp-port` | `ISB_WORKSPACE_MCP_PORT` | `8481` | where each org's workspace reaches the org-bound MCP, on its bridge address; must differ from the tunnel port |
| `--workspace-pool` | `ISB_WORKSPACE_POOL` | the org's default pool | the storage pool new workspace home volumes go in (an org's `home_pool` wins) |
| `--workspace-home-root` | `ISB_WORKSPACE_HOME_ROOT` | off | make workspace homes host folders, `<DIR>/<org>/home`, instead of volumes |
| `--preview-domain` | `ISB_PREVIEW_DOMAIN` | off (`.localhost` when isb is reached on loopback) | `[http(s)://]DOMAIN[:PORT]` whose subdomains reach the listener: workspace port previews get `<port>-<workspace>-<org>.DOMAIN` |

See [Workspaces](../concepts/workspaces.md).

### Sandbox egress

The proxy that serves sandboxes with `egress:` runs inside `isb serve`
([Sandbox egress and secrets](../guides/egress.md)); these are the operator's
settings for it.

| Flag | Environment | Default | |
|---|---|---|---|
| `--egress-pin` | `ISB_EGRESS_PINS` | none | `NAME=IP[:PORT]`, comma-separated: the proxy connects to `NAME` at that address instead of resolving it, and may reach a private one. The only way a name that resolves to a private address is reachable. |
| `--egress-ca` | `ISB_EGRESS_CA` | none | PEM files of certificates the proxy trusts for the hosts it connects to, besides the system's (a private CA) |

### Audit and history

| Flag | Environment | Default | |
|---|---|---|---|
| `--audit-retention` | `ISB_AUDIT_RETENTION` | `90d` | how long audit entries are kept |
| `--audit-all` | `ISB_AUDIT_ALL` | off: read-only tool calls are not recorded (secret reads and refusals always are) | record every call |
| `--history-retention` | `ISB_HISTORY_RETENTION` | `365d` | how long history rows are kept |
| `--history-max-rows` | `ISB_HISTORY_MAX_ROWS` | `5000000` | past it, the oldest history rows go first |

### Servers

| Flag | Environment | Default | |
|---|---|---|---|
| `--agent` | `ISB_AGENT` | off | run as a server's agent for a control plane: no identity store, web UI or `--listen`, an mTLS listener instead; needs the two below |
| `--agent-listen` | `ISB_AGENT_LISTEN` | | the agent's mTLS address, e.g. `0.0.0.0:7443` (any address: the client certificate is the gate) |
| `--agent-tls` | `ISB_AGENT_TLS` | | the agent's TLS directory: `ca.crt`, `tls.crt`, `tls.key` |

`isb server add` writes these into the agent's unit; see
[Servers and dedicated VMs](../guides/servers.md).

### `isb serve install`

| Flag | Default | |
|---|---|---|
| `--listen` | the env file's `ISB_SERVE_LISTEN`, else `127.0.0.1:8092` | the loopback address to serve on; given explicitly, it is written to the env file |
| `--machine` | `isb` | macOS: the machine the LaunchAgent starts |

## Environment variables

Everything else isb reads from its environment.

### Where things are

| Variable | Read by | |
|---|---|---|
| `INCUS_SOCKET` | every command | incusd's socket (the `--socket` flag) |
| `INCUS_DIR` | every command | without `INCUS_SOCKET`: the socket is `$INCUS_DIR/unix.socket` (else `/var/lib/incus/unix.socket`) |
| `INCUS_PROJECT` | every command | the incus project (the `--project` flag) |
| `ISB_ORG` | every command | the org (the `--org` flag) |
| `ISB_SERVE_SOCKET` | the CLI and the daemon | the daemon's unix socket |
| `ISB_SERVE_STATE_DIR` | the daemon, and `isb user`, `invite`, `token`, `key`, `audit`, `history`, `registry setup`, `org` | the daemon's state directory |
| `ISB_URL`, `ISB_TOKEN` | the CLI | a daemon's URL and an API token: `isb ssh-config`, `ssh-proxy` and `key` with a remote daemon, and every platform command where there is no local daemon socket (inside a workspace) |
| `ISB_PUBLIC_URL` | `isb invite` | prints the invitation as a link |
| `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, `HOME` | everything | the default state directory, `~/.config/isb`, the daemon socket and `isb up`'s locks |
| `ISB_DNS_DIR` | `isb org`, `isb host setup`, the daemon | the service-name hosts directory (default `/var/lib/isb/dns`) |
| `ISB_HOST_PATH_MAP` | `isb up`, `create` | `FROM=TO`: rewrite bind sources for incusd's mount view ([Bind source resolution](compose.md#volumes)) |
| `ISB_BIND_CALLER_OWNED` | `isb up`, the daemon | `1`: `idmap: auto` never maps, because every guest uid can write bind sources (set inside the macOS machine) |
| `SUDO_USER` | `isb host setup` | the default `--user` |

### Secrets

| Variable | |
|---|---|
| `ISB_AGE_KEY` | the daemon's age key itself (an `AGE-SECRET-KEY-1...` line, or a whole `age-keygen` file); first in the lookup |
| `CREDENTIALS_DIRECTORY` | systemd's credential directory: `isb-age-key` in it is the key (second) |
| `ISB_AGE_KEY_FILE` | a key file; it must exist when set (third). Last: `~/.config/isb/age.txt` |
| `ISB_SECRETS_CONFIG` | the break-glass recipients file (default `~/.config/isb/secrets.toml`) |
| `ISB_OP_BIN` | the 1Password CLI for the `onepassword` driver (default `op` on `PATH`) |
| `ISB_GITHUB_CLIENT_SECRET`, `ISB_GOOGLE_CLIENT_SECRET`, `ISB_OIDC_CLIENT_SECRET` | sign-in providers' client secrets |
| `ISB_S3_SECRET_KEY` | `isb backup dest create`: the secret key (else stdin's first line) |

See [Secrets](../guides/secrets.md#the-daemons-key).

### Builds and sources

Read by the daemon.

| Variable | Default | |
|---|---|---|
| `ISB_BUILD_TIMEOUT` | `30m` | a whole build, sandbox to push (`--timeout` per build wins) |
| `ISB_BUILD_CPUS` | `2` | a build sandbox's CPUs (counted against the org's quota) |
| `ISB_BUILD_MEMORY` | `4GiB` | a build sandbox's memory |
| `ISB_BUILD_CACHE_SIZE` | `20GiB` | a VM build's cache disk |
| `ISB_GIT_BIN` | `git` on `PATH` | the git that fetches app sources |

### Display

| Variable | |
|---|---|
| `NO_COLOR` | `isb tui` without colour (the glyphs carry the meaning) |
| `TERM` | passed to a command run with a terminal (else `xterm-256color`) |

### Building isb

| Variable | |
|---|---|
| `ISB_WEB_REQUIRED` | `1`: a build without `web/dist` fails instead of embedding a placeholder page |
| `ISB_WEB_DIST` | embed another directory as the web UI |

See [Developing the web UI](../contributing/web-ui.md). The variables isb's
own tests read are in [Developing isb](../contributing/index.md).

### Developing isb

Only debug builds (`cargo build`) honour these; a release build refuses to
run with either one set.

| Variable | Read by | |
|---|---|---|
| `ISB_DEV_WEAK_PASSWORDS` | `isb user`, the daemon | `1`: passwords of any length (not empty), so a dev daemon can have `dev@dev.com` / `password` |
| `ISB_DEV_SUPERADMIN` | the daemon | an email: every HTTP request with no credential (no session cookie, token or Access assertion) is a superadmin acting as that isb user (synthetic if there is none), audited as `dev:<email>`. The daemon refuses to start unless every `--listen` address is loopback |

## Variables isb sets

What isb puts in a guest's environment, for scripts to read:

| Variable | Where |
|---|---|
| `ISB_URL`, `ISB_ORG`, `ISB_TOKEN`, `ISB_WORKSPACE` | login shells in a workspace ([Workspaces](../concepts/workspaces.md#the-workspace-is-an-org-actor)) |
| `ISB_VOLUME`, `ISB_REASON`, `ISB_SNAPSHOT` | the `/etc/isb/pre-snapshot` hook ([Volumes](../guides/volumes.md)) |
| `ISB_TPL_<VAR>` | a template's command line or health check that uses a secret variable ([Templates](../guides/templates.md)) |
| `HOME`, `USER`, `LOGNAME` | `isb exec` and a service's `command`, from the user's passwd entry, unless given |

## Files

| Path | |
|---|---|
| `~/.config/isb/serve.env` | the daemon's environment under `isb serve install` |
| `~/.config/systemd/user/isb.service` | the daemon's unit |
| `~/.config/isb/age.txt`, `~/.config/isb/isb-age-key.cred` | the daemon's age key, plain or as an encrypted systemd credential |
| `~/.config/isb/secrets.toml` | break-glass recipients |
| `~/.config/isb/known_hosts` | host keys `isb ssh-config` pinned |
| `<state>/` | the daemon's state: stacks, orgs, secrets, `isb.db`, `audit.db`, metrics, ingress, registry CA ([The state directory](../operations/host-setup.md#the-state-directory)) |
| `/var/lib/isb/dns/<org>/` | service-name hosts files |
| `~/.isb/machine/NAME/` | macOS: the isb machine's files and forwarded sockets |
