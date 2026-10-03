---
title: HTTP API
description: The isb serve daemon's HTTP surfaces: MCP, REST tools, the event stream, the terminal and SSH websockets, the workspace resource and webhooks.
order: 4
---

`isb serve` answers on two kinds of listener, and every route on them leads
to the same [tools](mcp-tools.md) through the same authorizer. Use this
page when you are writing a client: an agent's MCP configuration, a script
calling REST, something following the event stream.

- **The unix socket** (`$ISB_SERVE_SOCKET`, else
  `$XDG_RUNTIME_DIR/isb/serve.sock`; 0600 in a 0700 directory) for the
  local `isb` CLI. Its callers are the daemon's own user, trusted with
  everything.
- **HTTP listeners** (`--listen`): loopback addresses, reached through a
  tunnel or reverse proxy, and tailnet addresses with
  `--superadmin-tailnet` ([Reach isb serve remotely](../guides/remote-access.md)).
  Each org with a workspace also gets an org-bound listener on its own
  bridge ([below](#the-org-bridge-listener)).

## Routes

| Path | What |
|---|---|
| `POST /mcp` | MCP (Streamable HTTP); every tool takes an `org` argument |
| `POST /orgs/<org>/mcp` | MCP bound to one org: `org` is filled in, and any other value is refused |
| `POST /api/v1/tools/<tool>`, `POST /orgs/<org>/api/v1/tools/<tool>` | REST: one tool call, the arguments as a JSON body |
| `GET /api/v1/tools` | the tools this listener offers, with their schemas and annotations |
| `GET /api/v1/openapi.json` | an OpenAPI document of the REST tools |
| `GET /api/v1/events` | server-sent events: deploys, rollouts, health and restarts in the caller's orgs |
| `GET /api/v1/audit/stream` | server-sent events: new audit entries the caller may read |
| `GET /api/v1/history/stream` | server-sent events: new history items the caller may read |
| `GET /orgs/<org>/api/v1/terminal?app=NAME` (or `?instance=NAME`) | a websocket to a shell ([below](#the-web-terminal)) |
| `GET /orgs/<org>/api/v1/ssh?instance=NAME` | a websocket carrying SSH, for `isb ssh-proxy` ([below](#the-ssh-websocket)) |
| `/orgs/<org>/api/v1/workspace[/ACTION]` | the org's workspace as a REST resource ([below](#the-workspace-resource)) |
| `POST /api/v1/webhooks/<org>/<app>` | an app's push and pull request webhooks ([Deploy apps](../guides/deploy-apps.md#webhooks)) |
| `GET /api/v1/templates/<catalog>/<id>/logo` | a template's logo, from isb's cached copy ([Templates](../guides/templates.md#logos)) |
| `/api/v1/auth/*` | identity: sign-in, sessions, invitations, tokens, keys ([Identity API](identity-api.md)) |
| `GET /healthz` | `{"ok": true, "isb": VERSION, "stacks": [{name, converged}]}`, without authentication |
| every other `GET` | the [web UI](../getting-started/web-ui.md), embedded in the binary; it never answers an API path |

## Signing in

Every HTTP caller is an isb user, or a superadmin:

- **API tokens**: `Authorization: Bearer isb_tok_...` (from `isb token
  create` or the web UI). How agents and scripts sign in. A workspace's
  token (`isb_ws_...`) is a bearer token too.
- **Sessions**: the `isb_session` cookie from the sign-in endpoints. How the
  web UI signs in. Cookie-authenticated writes must carry `X-Isb-Csrf: 1`.
- **Superadmin sources**: a superadmin token (`isb_sa_...`), a tailnet
  identity on a tailnet listener, or a verified Cloudflare Access identity on
  the allow lists. Tailnet and Access identities ride along like cookies, so
  their writes need `X-Isb-Csrf: 1`, their `/mcp` calls
  `Content-Type: application/json`, an `Origin`, when sent, must name the
  request's `Host`, and the `Host` must be one of this server's names.
- **Cloudflare Access**, when configured (`CF_ACCESS_TEAM_DOMAIN`,
  `CF_ACCESS_AUD`), guards the loopback listeners: every request needs a
  valid `Cf-Access-Jwt-Assertion`, and isb's own sign-in applies behind it.
  An Access identity whose email is an isb user's acts as that user. The
  webhook path and `/healthz` are served ahead of Access.
- **No credential**: refused (anonymous calls work only with
  `--allow-unauthenticated`, for local testing). A request with a credential
  that does not check out is a 401, even when a valid session cookie rides
  along.

What each caller may then do is in [Who may call
what](mcp-tools.md#who-may-call-what) and
[Users, roles and superadmins](../concepts/access.md).

## MCP

The daemon speaks MCP's Streamable HTTP transport with plain JSON
responses, statelessly:

- `POST` only: `GET /mcp` (no SSE stream) and other methods are 405.
- No session id; `tools/call` works without a prior `initialize`.
- Methods: `initialize` (protocol versions `2025-11-25`, `2025-06-18`,
  `2025-03-26`; an unknown one is answered with the newest), `ping`,
  `tools/list` (never paginated), `tools/call`. Notifications are accepted
  and answered with 202. JSON-RPC batches (arrays) work.
- A refused or failed tool call is a tool result with `isError: true` and
  `structuredContent: {code, message, data}`.

```sh
curl -s https://isb.example.com/orgs/acme/mcp \
  -H "Authorization: Bearer $ISB_TOKEN" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

## REST tools

`POST /api/v1/tools/<tool>` (or `/orgs/<org>/api/v1/tools/<tool>`) with the
arguments as a JSON object body (an empty body is `{}`):

```sh
curl -s https://isb.example.com/orgs/acme/api/v1/tools/app_deploy \
  -H "Authorization: Bearer $ISB_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "web", "wait": true}'
```

Success is `200 {"result": ...}`. Failure is `{"error": CODE, "message":
TEXT, "data": ...}` with a status from the code:

| Status | Codes |
|---|---|
| 400 | `invalid`, `parse`, `interpolation`, `bad_request`; a body that is not a JSON object |
| 401 | `unauthorized`: a credential that does not check out |
| 403 | `forbidden`: not allowed, or a CSRF/`Origin` check failed |
| 404 | `not_found`, and an unknown or hidden tool |
| 409 | `already_exists` |
| 504 | `request_timeout`, `operation_timeout`, `exec_timeout`, `not_ready` |
| 500 | everything else |

`GET /api/v1/openapi.json` describes these calls; the web UI's typed client
is generated from it.

## Events

`GET /api/v1/events` is a stream of server-sent events: deploys, rollouts,
health changes, restarts, failures, backups, jobs, certificates, and each
deployment log line (level `log`), in the caller's orgs only. Each event has
`id: <seq>`, `event: <level>` and the event as JSON `data`. It resumes from
`Last-Event-ID` or `?since=SEQ`, and sends a keepalive comment when quiet.
The `events` tool is the same feed for clients that poll.

`GET /api/v1/audit/stream[?org=ORG][&after=ID]` streams new audit entries
(`event: audit`, the entry's id as `id`) from `after`, `Last-Event-ID` or
now, and `GET /api/v1/history/stream[?org=ORG]` new history items
(`event: history`; the event id is the cursor to resume from). Both need a
session or a token, show only what the caller may read, and send keepalives
every 15 s. See [The audit log](../operations/audit.md) and
[The history](../operations/history.md).

## The web terminal

`GET /orgs/<org>/api/v1/terminal?app=NAME[&slot=N][&cols=C&rows=R]`
upgrades to a websocket bridged to a login shell (bash, else sh) in one of
the app's running replicas (`slot`, or one in rotation), with a
pseudo-terminal. With `?instance=NAME` instead of `app`, the shell is in
that instance of the org (a workspace, a sandbox), as root. The web UI's
Terminal tabs use it.

- **Who**: the caller signs in as for any tool and is admitted as if calling
  `sandbox_exec` in the org: its members, admins and owners (not viewers,
  nor tokens whose scopes leave out `sandbox_exec`), platform admins, and
  nobody when `--deny-tools` covers `sandbox_exec`. Only the org's own apps
  and instances are reachable.
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

## The SSH websocket

`GET /orgs/<org>/api/v1/ssh?instance=NAME[&as=EMAIL]` upgrades to a
websocket whose binary frames are an SSH connection's bytes; the daemon runs
`sshd -i` in the instance through incus exec and bridges the two. It is
admitted exactly like the web terminal. `as` names whose isb SSH keys to let
in, for the unix socket and superadmin tokens, which have no account of
their own; anyone else may only name themselves. 64 sessions per daemon,
closed after 2 hours idle and 24 hours in all. `isb ssh-proxy` is its
client; see [SSH and herdr](../guides/ssh.md).

## The workspace resource

The org's workspace as REST, over the `workspace_*` tools
([Workspaces](../concepts/workspaces.md)):

| Method and path (`/orgs/<org>/api/v1/...`) | Tool |
|---|---|
| `GET workspace[?name=]` | `workspace_get` |
| `POST workspace` | `workspace_create` |
| `PATCH workspace` | `workspace_update` |
| `DELETE workspace` (body `{"confirm": true}`) | `workspace_delete` |
| `POST workspace/start`, `/stop`, `/restart`, `/rebuild`, `/token/rotate` | the action |
| `GET`, `PATCH workspace/settings` | `workspace_settings` |

Bodies and answers are as for the REST tools.

## The org-bridge listener

For each org with a workspace, the daemon serves the org-bound surface on
the org bridge's gateway address, `http://<gateway>:8481`
(`--workspace-mcp-port`). That is `$ISB_URL` inside the workspace.

- It answers only peers in the org's own subnet (others get 403).
- It serves only `/orgs/<org>/...` (MCP, the REST tools, the workspace
  resource, the terminal and SSH websockets) and `/healthz`; every call is
  pinned to the org.
- It takes bearer tokens only (the workspace's, or an org API token): no
  session cookies, no Access assertions, no superadmin tokens.

See [Reaching isb from inside](../concepts/workspaces.md#reaching-isb-from-inside-the-bridge-listener).

## Agents' listener (servers)

A server's agent (`isb serve --agent`) has no public API: its mTLS listener
admits only the control plane's client certificate, and each request carries
the control plane's assertion of the caller (`Authorization: IsbAssert
<base64url JSON>`). It serves the org-scoped routes above for the orgs
placed on it, plus internal routes the control plane uses
(`/internal/v1/heartbeat`, `/internal/v1/orgs`, `/internal/v1/cert`). See
[Servers and dedicated VMs](../guides/servers.md#mtls).

## Audit

Every call that changes something, every refusal, every secret read,
sign-in, webhook delivery and terminal or SSH session is recorded in the
[audit log](../operations/audit.md), with the surface it came through
(`mcp`, `rest`, `web`, `cli`, `webhook`). Every call is also logged to the
daemon's stderr with the caller, the tool, its duration and whether it
failed, never its arguments.
