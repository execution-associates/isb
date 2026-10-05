---
title: Agents and MCP
description: Connect an AI agent to isb over MCP, with a token that reaches one org, and let agents in an org's workspace make their own sandboxes.
order: 14
---

Everything isb's web UI does is a tool on `isb serve`'s
[Model Context Protocol](https://modelcontextprotocol.io) server: deploy an
app, read its logs, roll it back, make a sandbox and run commands in it,
manage secrets and backups. An agent such as Claude Code, Codex or Cursor
connects to it like any remote MCP server. The point is reach without risk:
an agent holding an org's token administers that org's apps and sandboxes and
nothing else, and the [remote-spec
policy](../concepts/security.md#the-remote-spec-policy) keeps even a careless
or compromised agent off the host.

## Two endpoints

| Endpoint | For | Every tool's `org` |
|---|---|---|
| `/orgs/ORG/mcp` | an agent working in one org: the usual choice | filled in; any other value is refused |
| `/mcp` | a caller that spans orgs (a platform admin's token, a superadmin) | an argument the caller passes |

Both serve the same tools, filtered to what the caller may use. The org-bound
endpoint is what an org token should point at: it cannot be talked into
another org, whatever the arguments say. It is scoped to the org even for a
superadmin or a platform admin, who act there as an admin of that org only:
the host, superadmin and platform tools are not listed and are refused, and
the remote-spec policy applies. A superadmin can therefore add `/mcp` and
`/orgs/ORG/mcp` connectors side by side, each behaving as its URL says. Each tool's refusal is an MCP tool
error with the reason, so the agent can explain it.

The transport is MCP's Streamable HTTP with plain JSON responses (no SSE
stream; `GET /mcp` is 405), stateless: `tools/call` works without a prior
`initialize`, and there is no session id. The same tools answer as REST at
`POST /orgs/ORG/api/v1/tools/<tool>` ([HTTP API](../reference/http-api.md)),
and [MCP tools](../reference/mcp-tools.md) lists them all.

## Connect an agent to an org

1. **Make a token for the org.** In the web UI, open the org's **MCP** page
   (`/orgs/ORG/agents`) and make a token there, or on the host:

   ```sh
   isb token create claude-code --org acme --expires 90d
   ```

   It is shown once. Any member of the org can make one for it; it acts with
   that member's role, read on every request, so demoting or removing the
   member narrows or ends the token at once.
2. **Give it to the client** through an environment variable, never as a value
   in a file you commit. With the token in `ISB_TOKEN`:

   ```sh
   export ISB_TOKEN=isb_tok_...
   claude mcp add --transport http --scope user isb-acme https://isb.example.com/orgs/acme/mcp \
     --header "Authorization: Bearer $ISB_TOKEN"
   ```

   ```toml
   # ~/.codex/config.toml
   [mcp_servers.isb-acme]
   url = "https://isb.example.com/orgs/acme/mcp"
   bearer_token_env_var = "ISB_TOKEN"
   ```

   ```json
   {
     "mcpServers": {
       "isb-acme": {
         "url": "https://isb.example.com/orgs/acme/mcp",
         "headers": { "Authorization": "Bearer ${env:ISB_TOKEN}" }
       }
     }
   }
   ```

   The last is Cursor's `~/.cursor/mcp.json`; other clients that take an
   `mcpServers` map use the same shape. For a repository, Claude Code's
   `.mcp.json` takes the same map with `"type": "http"` and `${ISB_TOKEN}`
   references, so the file can be committed and the token stays outside it.
3. **Check it** without the agent:

   ```sh
   curl -sS https://isb.example.com/orgs/acme/mcp \
     -H "Authorization: Bearer $ISB_TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
   ```

   A JSON answer with `tools` means the URL and the credentials work. 401: the
   token is missing, wrong, revoked or expired (the same call without the
   `Authorization` header answers `401` with `sign in: send an API token as
   Authorization: Bearer ...`); 403 from Cloudflare: Access stopped it before
   isb.

The MCP page writes all of these for you: the org endpoint's URL on the origin
the page is open at, a form that makes the token (name, access, expiry) and
fills the `export` line with it, copyable snippets for Claude Code (`claude
mcp add` and `.mcp.json`), Codex (`codex mcp add` and `config.toml`), Cursor
and other `mcpServers` clients, the `curl` test, and the tools the endpoint
lists (`GET /api/v1/tools`). Who may make a token there follows the server's
rules: members, viewers (whose token only reads), platform admins; not a
superadmin without an account, and not a session signed in with a narrowed
token.

### Without a token: tailnet and Access identities

An org's owners and admins can let an agent in by where it connects from,
with a role in that org only ([Agent identities](../concepts/access.md#agent-identities)).
The MCP page's **How the agent signs in** shows three cards, each On or Off,
and the snippets for the one selected:

| Card | On when | The agent |
|---|---|---|
| Org token | always | sends `Authorization: Bearer` (above) |
| Tailnet identity | the server has a tailnet `--listen` address and the org maps a tailnet login or tag | connects to `http://TAILNET-ADDRESS:PORT/orgs/ORG/mcp` from a mapped node, with no credential |
| Access identity | Access guards a listener, and the org maps an Access service token or email, or has member users | sends the Access service token's `CF-Access-Client-Id` and `CF-Access-Client-Secret`; a person whose email is a member acts as that member |

An Off card says what is missing: a tailnet `--listen` address on the server,
or a mapping set in the org's **Settings, Agent identities** (or
`agent_identity_set`). The client must send `Content-Type: application/json`
and no foreign `Origin`, as MCP clients do; REST writes need `X-Isb-Csrf: 1`.

### Behind Cloudflare Access

When the daemon sits behind Access ([Reach isb serve
remotely](remote-access.md)), an agent also needs to get past Access:

- **claude.ai and Claude Desktop connectors** (and ChatGPT's) use Access
  Managed OAuth: add `https://isb.example.com/mcp` (or the org URL) as a
  connector and sign in through Access in the browser. No isb token is
  copied; the Access identity acts as the isb user with that email.
- **Command-line agents** send an Access service token's headers as well,
  `CF-Access-Client-Id` and `CF-Access-Client-Secret`, read from
  `CF_ACCESS_CLIENT_ID` and `CF_ACCESS_CLIENT_SECRET`. The MCP page's
  **Access** switch adds them to every snippet; it is on by default for an
  address that is not localhost or the tailnet.

## Narrow what a token may do

A token with no scopes has its user's whole role in the org: an agent
administers the org's apps as its people do. Scopes narrow it, and never widen
a role:

| Scope | Allows |
|---|---|
| `read` | read-only tools, never secret material (as a viewer) |
| `deploy` | `read`, plus `stack_deploy`, `stack_redeploy`, `stack_rollback`, `stack_scale`, `app_scale`, `app_restart`, `instance_restart`, `app_deploy`, `app_rollback`, `build_run` |
| `admin` | everything the role allows (the same as no scopes) |
| `tool:GLOB` | tools whose name matches, e.g. `tool:app_*`, `tool:stack_status` |

```sh
isb token create ci --org acme --scope deploy
isb token create watcher --org acme --scope read --scope 'tool:app_deployment_log'
```

The MCP page and the Account page offer the same choice as **Access**: full,
deploy, read only, or only some tools. A scoped token without `admin` cannot
change accounts, tokens, invitations or members. Details: [API tokens and
scopes](../concepts/access.md#api-tokens).

The operator can also hide tools from every remote caller with
`--allow-tools` and `--deny-tools` (names or globs such as `sandbox_*`,
comma-separated; deny wins). A hidden tool is not listed and cannot be
called. The local socket always has every tool.

```dotenv
# ~/.config/isb/serve.env: remote callers never read secret values or exec
ISB_SERVE_DENY_TOOLS=secret_*,sandbox_exec,app_exec,stack_exec,instance_exec,instance_file_*
```

## What an agent can and cannot do

An org's member token reaches everything in the org: its apps, deployments,
stacks, sandboxes (create, exec, remove), secrets (values included),
databases, backups, volumes, jobs, notifications and, for owners and admins,
the audit log. It reaches nothing in any other org, and no platform or host
tool.

It can also look inside what runs, as `kubectl` does for pods:
`instance_list` and `instance_get` describe every instance of the org,
`app_exec`, `stack_exec` and `instance_exec` run a command in a replica (or any instance),
`app_logs`, `app_top` and `app_events` read an app's output, use and events,
`app_restart`, `app_scale` and `instance_restart` roll, resize and replace
replicas, and `instance_file_read` and `instance_file_write` copy small files
in and out. Viewers and `read` tokens only read what `instance_list`,
`instance_get`, `app_logs`, `app_top` and `app_events` show; running code,
restarting and files are for members and up. [isb for kubectl
users](kubectl.md) maps each to the `kubectl` verb it replaces.

Whatever the role, a remote caller's specs are held to the remote-spec
policy: no privileged containers, raw config or devices, host bind mounts
outside the operator's `--bind-root` directories, ports published on
anything but loopback, or instances isb does not manage, unless the operator
allows each one. `${VAR}` in a remote caller's compose file is filled from the
`vars` it sent, never from the daemon's environment. See
[Security](../concepts/security.md#the-remote-spec-policy).

Every call that changes something, and every refusal, is in the [audit
log](../operations/audit.md) with the token's name; a sandbox a remote caller
creates is labelled `isb.owner=mcp:<identity>`.

## Agents in an org's workspace

An org's [workspace](../concepts/workspaces.md) is its long-lived machine,
where its people and agents work. It comes with its own credential, so an
agent started inside needs no setup:

- `/run/isb/token` (0400, owned by the workspace user) holds the workspace's
  token, `isb_ws_...`, with a role in the org (`--token-role`, admin by
  default). It is never shown by any tool, never logged, and rotated with
  `isb workspace rotate-token`.
- Login shells get it as `$ISB_TOKEN`, with `$ISB_URL` (where isb answers),
  `$ISB_ORG` and `$ISB_WORKSPACE`, from `/etc/profile.d/isb.sh`.
- `$ISB_URL` is the org bridge's gateway on port 8481
  (`--workspace-mcp-port`), the **bridge listener**: it answers only the
  org's own subnet, serves only `/orgs/<org>/...` and `/healthz`, and takes
  only bearer tokens (the workspace's or an org API token; no cookies, Access
  assertions or superadmin tokens). See [Reaching isb from
  inside](../concepts/workspaces.md#reaching-isb-from-inside-the-bridge-listener).

So inside the workspace:

```sh
claude mcp add --transport http --scope user isb "$ISB_URL/orgs/$ISB_ORG/mcp" \
  --header "Authorization: Bearer $ISB_TOKEN"
isb app ls                     # the isb CLI uses $ISB_URL and $ISB_TOKEN when there is no daemon socket
```

The workspace's Connect tab shows the same snippets, the token's role and last
use, and Rotate for admins (never the token itself). In the audit log and the
history the workspace acts as **`workspace`**.

The token is confined to its org: it acts as no user, reaches no other org,
no platform or host tool, and cannot use the identity endpoints (no members,
invitations, tokens or keys).

### Sandboxes for heavy or risky work

The workspace is shared and long-lived, so builds, test runs and anything
untrusted belong in sandboxes the agent makes through the same MCP, never in
nested incus. `sandbox_create` takes one compose service as the spec:

```json
{"name": "sandbox_create", "arguments": {
  "spec": {"container_name": "pr-412-tests", "image": "dev-base",
           "cpus": 2, "mem_limit": "4g", "user": "dev"},
  "expires": "6h", "idle_timeout": "1h"}}
```

then `sandbox_exec` runs argv in it (exit code, stdout and stderr each capped
at 256 KiB keeping the end, optional stdin text, a timeout of 10 minutes by
default), and `sandbox_remove` deletes it. A sandbox made this way is
labelled `isb.owner=workspace` and is short-lived by rule:

- it **expires** after the org's `sandbox_expiry` (default 24 h, at most 30
  days), or the call's `expires`;
- it is **deleted when idle** for the org's `sandbox_idle` (default 2 h), or
  the call's `idle_timeout`, or never with `none`. Idle means no exec or
  terminal through isb and under 2 % of a core of CPU.

`sandbox_extend` pushes the expiry out (`by`, default 24 h) or changes the idle
timeout; the sandbox's creator or the org's admins may. `sandbox_list` shows
each sandbox's creator, age, expiry and use, and whether the caller made it
(`mine`). The daemon's reaper deletes expired and idle sandboxes every minute
and records `sandbox.reaped` in the history. The workspace itself can never
be reached as a sandbox.

## Superadmin MCP

A superadmin has the unix socket's reach over HTTP: every tool, no
remote-spec policy, any instance on the host, and the host tools
(`host_inventory`, `host_policy`, `superadmin_token_list`,
`superadmin_token_revoke`). It is for agents that administer the host itself,
across orgs, and it is root on the host in all but name. It comes from exactly
three remote sources ([Superadmins](../concepts/access.md#superadmins)):

- a **superadmin token**, `isb token create NAME --superadmin [--expires
  30d]`, minted only on the host (no HTTP caller can mint one), sent as
  `Authorization: Bearer isb_sa_...`;
- a **tailnet identity** on a tailnet `--listen` address, listed in
  `--superadmin-tailnet`: the agent's machine signs it in, so no token is
  needed;
- a **Cloudflare Access identity** listed in `--superadmin-access`.

Point it at the unbound `/mcp`: on `/orgs/ORG/mcp` a superadmin is scoped
down to an admin of that org. For a superadmin the MCP page opens on two
tabs, **Superadmin (/mcp)** first and selected, and **This org
(/orgs/ORG/mcp)**, each linkable as `/orgs/ORG/agents?endpoint=superadmin`
or `?endpoint=org`. The superadmin tab shows this endpoint with a warning,
and for each source whether it is on, its URL and its snippets (the client
configs and the `curl` test); the org tab is the page everyone else sees,
which has no tabs. The web never makes a superadmin token. Tailnet and Access
superadmins are ambient credentials, so their `/mcp` calls must be
`Content-Type: application/json` and a foreign `Origin` is refused.

Prefer an org token wherever one org is enough.

## Locally, without the daemon's HTTP

On the host itself an agent can use the `isb` CLI directly (it talks to the
daemon's unix socket, as the daemon's user, with every tool), or run sandboxes
from an `isb.yaml` with `isb up` ([A dev environment per
worktree](dev-environments.md)). The agent skill in the repository's
[`SKILL.md`](https://github.com/execution-associates/isb/blob/main/SKILL.md) teaches an agent both.
