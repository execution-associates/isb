---
title: Users, roles and superadmins
description: Who may do what in isb serve, from an org viewer to a superadmin with the host's reach.
order: 8
nav_title: Users and roles
---

`isb serve` keeps its own users. Every HTTP caller is one of them (or an
agent holding one of their tokens), and what a caller may do follows from
two things: its role in an org, and whether it is a platform admin or a
superadmin. This page explains those levels; signing in is in
[Sign-in](../guides/sign-in.md), and the endpoints are in the
[Identity API](../reference/identity-api.md).

| Level | Reaches | Typical holder |
|---|---|---|
| org viewer | reads one org | a stakeholder, a dashboard |
| org member, admin, owner | administers one org | the org's people, its workspace, its agents |
| org agent identity | one org, at a role the org gave it (never owner) | a tailnet node or Cloudflare Access service token an org maps ([Agent identities](#agent-identities)) |
| platform admin | every org, plus creating and deleting orgs | the platform's operators |
| superadmin | everything the daemon's unix socket can do, the host included | the host's own operator, break-glass automation |

## Roles

**The org is the trust boundary**: a member of an org (member, admin or
owner) fully administers that org's apps, stacks, sandboxes and secrets, and
nothing in any other org. Each membership has a role:

| Role | List and inspect (read-only tools, logs, events) | Administer the org's apps and secrets: deploys, exec and the terminal, secret values | Manage members, invitations, every token in the org; read its audit log | Make and change owners |
|---|---|---|---|---|
| `viewer` | yes | | | |
| `member` | yes | yes | | |
| `admin` | yes | yes | yes | |
| `owner` | yes | yes | yes | yes |

A viewer runs only tools annotated read-only (`readOnlyHint`) that hand out
no secret material: it can list and inspect stacks, apps, deployments, logs,
secret names and the org, and is refused writes, deploys, `sandbox_exec`
(and so the web terminal and SSH), `secret_get`, `secret_resolve` and
`app_webhook`. The check is the one authorizer every surface goes through, so
new tools are covered by their annotation.

- An actor grants roles up to its own: an admin can add viewers, members and
  admins; only an owner (or platform admin) can make or change an owner.
- An org always keeps one owner: the last one cannot be demoted or removed.
- Anyone can leave an org.
- Roles are stored as text and mapped to permissions in one table in code
  (`Role::permissions`), so a new role or a finer permission needs no
  migration.

Some org tools need more than membership: volume snapshots, schedules and
restores are for the org's admins and owners
([Volumes](../guides/volumes.md)), and so are creating, changing and deleting
the workspace ([Workspaces](workspaces.md#who-may-do-what)). The audit log
is for owners and admins ([The audit log](../operations/audit.md)).

## Agent identities

An org can let agents in with no isb token, by where they connect from. An
org's owners and admins map identities to a role in **that org**
(`agent_identity_set`, or Settings, Agent identities in the web UI):

| Front door | A mapping names | Matches |
|---|---|---|
| tailnet | a login (`someone@example.com`) or a node tag (`tag:agents`) | a peer on a tailnet `--listen` address, as tailscaled's `whois` says: a tagged node by its tags only, never its owner's login; any other node by its user's login |
| Cloudflare Access | a service token's client id, or the email of someone who is not an isb user | a verified `Cf-Access-Jwt-Assertion` on a listener Access guards |

- The role is `viewer`, `member` or `admin`, never `owner`, and at most the
  role of whoever sets it. Names are exact (emails, logins and tags
  case-insensitively; client ids as given), with no wildcards or domains.
  `agent_identity_list` shows them to any member; viewers read, owners and
  admins change. Setting and removing are audited
  (`auth.agent_identity_set`, `auth.agent_identity_remove`).
- A matched caller is a synthetic principal named after its source,
  `tailnet:<login>` (a tagged node: `tailnet:<node>`) or `access:<email or
  client id>`, with no isb account (no sessions, passkeys or tokens;
  `POST tokens` is `403`). It holds a role in each org that maps it and
  nothing anywhere else: a tool call for another org is refused, `/orgs/<org>/mcp`
  pins its own org, and the platform, `server_*`, org-management and host
  tools are never reachable. It is never a superadmin; an identity on the
  superadmin lists is judged as a superadmin first.
- The mapping alone decides. A tailnet login that is also an isb user's
  email is still the mapped principal, not that user, and gets nothing of the
  user's memberships. An Access email that is an isb user's is the opposite:
  it acts as that user with their real memberships, and cannot be mapped (the
  mapping is refused, and ignored if the account came later); make the user a
  member instead. The web form offers that inline ("Add the user to the org as
  the chosen role") to owners and admins when the refusal names an isb user.
- Some callers get in through a front door with no mapping, and the org's MCP
  page counts them when it says whether Access or the tailnet sign-in is on:
  a member user's Access email, a platform admin's (who reaches every org),
  an identity on `--superadmin-access`, and a login on `--superadmin-tailnet`.
  `agent_identity_list` reports them under `available.reach` as counts for any
  member, with the names (emails, logins, client ids) only for the org's
  owners, admins and platform admins, plus whether the caller is one. It also
  reports `available.public_url`, the address Access sits in front of, which
  the Access card shows as `<public URL>/orgs/<org>/mcp`.
- A request that carries a bearer token or a session cookie is judged by it
  alone; the identity is asked only of one that carries neither. The caller
  gets a role in each org that maps it, the highest if several of its tags do.
- It is **ambient**, like a cookie, so the superadmin defences apply: writes
  need `X-Isb-Csrf: 1`, `/mcp` needs `Content-Type: application/json`, an
  `Origin` must name the `Host`, and the `Host` must be one of the server's
  names (the tailnet listen addresses, the node's MagicDNS names, the public
  URL's host). Refusals are audited (`agent.refused`). The tailnet identity
  comes from the TCP peer only, never a forwarded header.
- Workspace tokens are unchanged: they stay the org's own actor.

## Platform admins

A **platform admin** (a flag on the user) can do everything in every org, and
alone creates and deletes orgs and changes their limits and egress
exceptions ([Orgs](orgs.md#from-the-api-and-the-web-ui)): those are what keep
orgs apart, so an org's own owners see them read-only. The first user
created is always a platform admin and owner of the `default` org.

Platform admins also:

- manage users from the web UI's Platform page or the `admin/users`
  endpoints: disable and enable them, and make or unmake platform admins.
  Nobody does either to themselves, and an enabled platform admin always
  remains (creating users, setting passwords, and minting tokens or adding
  SSH keys for someone are on the host only, `isb user` and `isb token`);
- are the only callers of `server_status`, `org_list`, `org_create`,
  `org_update`, `org_delete`, the `server_*` tools, `audit_verify`,
  `registry_gc`, `notification_settings`, adding and removing template
  catalogs, and re-encrypting every org's secrets, whatever their role in an
  org (an org owner's token is refused);
- see every org's audit log and history, host-level rows included.

A platform admin is still a remote caller: its specs are held to
[the remote-spec policy](security.md#the-remote-spec-policy), and it cannot
reach instances isb does not manage. That is the line between a platform
admin and a superadmin.

## Superadmins

A **superadmin** has what the daemon's unix socket has: every tool, no
remote-spec policy, any instance, plus the host tools, which nobody else
gets, platform admins included:

| Host tool | Does |
|---|---|
| `host_inventory` | every incus project and instance on the host, isb's or not: project, org, type, status, addresses, isb's stack and owner labels |
| `host_monitor` | live resource use of the host or one remote server (CPU per core, load, memory and swap, pools, disk I/O, interfaces with addresses and rates, the last hour) and each instance's rates; the web UI's **Monitor** page |
| `host_policy` | how the daemon serves: listen addresses, Access, the remote tool policy, what remote specs may ask for, and each superadmin source with its allow list and token count |
| `superadmin_token_list`, `superadmin_token_revoke` | superadmin tokens' metadata, and revoking one by id |
| `superadmin_list` | every tailnet and Access superadmin identity, from the flags and from `isb.db`, with whether the daemon can match it (read only) |

Four sources grant it, and nothing else:

| Source | Who | Audit name |
|---|---|---|
| the unix socket | the daemon's own user | `local(uid N)` |
| a superadmin token | `Authorization: Bearer isb_sa_...` | `token:<name>` |
| a tailnet identity | a peer on a tailnet `--listen` address whose login or node tag is on `--superadmin-tailnet` or added with `isb superadmin add --tailnet` | `tailnet:<login>` (a tagged node: `tailnet:<node>`) |
| a Cloudflare Access identity | a verified `Cf-Access-Jwt-Assertion` whose email or service token client id is on `--superadmin-access` or added with `isb superadmin add --access` / `--access-token` | `access:<name>` |

A debug build has one more, for developing isb: `ISB_DEV_SUPERADMIN`, which
makes every loopback request with no credential a superadmin, `dev:<email>`
([Developing isb](../reference/configuration.md#developing-isb)). A release
build refuses to start with it set.

`isb serve` logs at start-up which superadmin sources are on, each list
with where it comes from (the flag, or `isb.db`), and the web
UI's **Host** pages show them ([The web UI](../getting-started/web-ui.md)).
Turning on the tailnet and Access sources is in
[Reach isb serve remotely](../guides/remote-access.md).

### Superadmin tokens

Superadmin tokens belong to nobody. They are minted only on the host, by the
daemon's user, which writes `isb.db` directly:

```sh
isb token create break-glass --superadmin --expires 30d   # prints the token once
isb token ls                                               # superadmin tokens as sa-ID, SUPERADMIN for a user
isb token revoke sa-3
```

`--superadmin` takes no `--org`, `--user` or `--scope`. No HTTP caller can
mint one, a superadmin included (`POST tokens` with `"superadmin": true` is
`403`), so a stolen HTTP credential never turns into a durable one. They are
revoked with `isb token revoke sa-ID`, the web UI's Host page, or
`superadmin_token_revoke`. Names are unique, 1 to 64 of `[A-Za-z0-9._-]`.
Minting and revoking are in the audit log.

### Superadmin identities in isb.db

Tailnet and Access superadmins come from two places, and both count:

- the flags `--superadmin-tailnet` and `--superadmin-access`, read at
  start-up: the bootstrap, for the first superadmin of a new host;
- identities kept in `isb.db`, added and removed on the host while the
  daemon runs. The daemon reads them on each request that could match one,
  so a change takes effect at the next request, with no config edit and no
  restart:

```sh
isb superadmin add --access alice@example.com     # an Access user, by email
isb superadmin add --access-token abc123.access   # an Access service token, by client id
isb superadmin add --tailnet tag:ops              # a tailnet login or node tag
isb superadmin ls                                 # flags and isb.db, with SOURCE and EFFECTIVE
isb superadmin rm --access alice@example.com
```

Like superadmin tokens, they are written only by the daemon's user on the
host, which opens `isb.db` directly; no HTTP endpoint or tool writes them, a
superadmin included, so an HTTP credential never makes itself or anyone
else a durable superadmin. Values are exact, as the flags take them: emails
and logins match case-insensitively, a client id exactly, a tag only a
tagged node; an empty value, a wildcard, a comma or a duplicate is refused.
An Access entry matches only where the daemon has Access and `--public-url`,
a tailnet entry only where it has a tailnet `--listen` address; elsewhere
`ls`, `superadmin_list` and the start-up log mark it not effective, and
`add` warns. An entry from a flag is removed from the flag. Adding and
removing are in the audit log (`auth.superadmin_add`,
`auth.superadmin_remove`).

### Tailnet and Access superadmins

- A tailnet or Access superadmin whose login or email is an enabled isb user
  acts as that user (its memberships, its account pages) plus superadmin.
  Anyone else, a superadmin token and a tagged node or service token
  included, is a synthetic principal named after its source, with no account
  of its own (no sessions, passkeys or tokens; `POST tokens` is `403`).
- Both are **ambient**, like a cookie: a browser on a tailnet machine, or one
  holding the `CF_Authorization` cookie, sends them with any page's request.
  So, for them:
  - writes must carry `X-Isb-Csrf: 1`;
  - `/mcp` must be `Content-Type: application/json` (a cross-site page sends
    that only after a CORS preflight, which isb never grants);
  - an `Origin`, when sent, must name the request's `Host`;
  - the `Host` must be one of this server's names (the listen addresses, the
    public URL's host, the node's MagicDNS names), which blocks DNS
    rebinding.

  The web terminal's `Origin` check applies as for a session. A request that
  fails these is refused and recorded (`superadmin.refused`). An MCP client
  that sends a foreign `Origin` is refused as a superadmin.
- A **tailnet** identity comes only from the TCP peer address, asked of the
  local tailscaled (`whois` over its LocalAPI socket, else the `tailscale`
  CLI on macOS), cached for a minute per address. Forwarded headers
  (`X-Forwarded-For`, `Tailscale-User-Login`) are never read. A tagged node
  is its tags, never its owner's login. When tailscaled does not answer,
  nobody is a tailnet superadmin, and the daemon says why once.
- An **Access** identity counts only from an assertion the daemon verified,
  on the loopback listeners Access guards; emails match case-insensitively
  and exactly, service tokens by client id, with no wildcards or domains.
  `--superadmin-access` is refused without Access configured and without
  `--public-url` (an Access superadmin's `Host` must be it); an Access
  identity in `isb.db` matches only when both are there.
- A request carrying a bearer token is judged by the token alone; a tailnet
  or Access superadmin is signed in ahead of any session cookie, and signing
  out does not change who it is.
- Every call a superadmin makes over HTTP is recorded, reads included, with
  actor kind `superadmin` and its source as the actor; refusals too. A
  sandbox it creates is labelled `isb.owner=<source>`.

### On an org-bound endpoint

A superadmin's reach is the unbound `/mcp` and `/api/v1/tools/<tool>`. On an
org-bound endpoint (`/orgs/<org>/mcp`, `/orgs/<org>/api/v1/tools/<tool>`, the
terminal, SSH and workspace resources under `/orgs/<org>/`) a superadmin over
HTTP, and a platform admin, acts as an **admin of that org only**, so an org
connector in an MCP client behaves as an org connector whoever adds it:

- `tools/list` there is an org admin's list: the host, superadmin and platform
  tools are absent, and calling one is refused.
- The remote-spec policy applies (no privileged containers, raw incus config,
  host binds or instances isb does not manage), and `org` is pinned.
- A platform admin or superadmin gets the admin role in that org. A user who is
  also a member keeps the higher of their own role and admin, so an owner stays
  an owner; everyone else keeps their own role.
- The audit row names who it really was (a superadmin's source, or the user)
  and records `scope: org <org>` in its details; reads are recorded too.
- The unix socket is always full, on any path. Workspace tokens, org tokens
  and agent identities are unchanged.

## How callers sign in

| Credential | Used by | Notes |
|---|---|---|
| Session cookie `isb_session` | the web UI | from password, passkey or provider sign-in; cookie-authenticated writes carry `X-Isb-Csrf: 1` |
| API token, `Authorization: Bearer isb_tok_...` | agents, scripts, the CLI against a remote daemon | from `isb token create` or the web UI |
| Workspace token `isb_ws_...` | the agents in an org's workspace | delivered inside the workspace, never shown ([Workspaces](workspaces.md#the-workspace-is-an-org-actor)) |
| Cloudflare Access | anyone, when Access is in front | an Access identity whose email belongs to an isb user acts as that user; one that does not gets "ask an org admin to invite you" |
| Tailnet or Access agent identity | agents an org maps | [Agent identities](#agent-identities) |
| Superadmin sources | operators | above |

Anonymous callers are refused, unless the daemon runs with
`--allow-unauthenticated` (local testing only): a request with no credential
to `/mcp`, `/orgs/<org>/mcp`, `/api/v1/tools` or the events stream gets `401`
with `WWW-Authenticate: Bearer`, so not even the tool list is shown. The unix
socket needs no token; the machine's users are its callers.

## API tokens

`Authorization: Bearer isb_tok_...`. A token belongs to a user and is
either:

- **org-scoped**: confined to one org, with the user's current role in it
  (read on every request, so a demotion or removal applies at once; leaving
  the org deletes the user's tokens for it). Any member can make one for
  their own org. A platform admin's org token acts as owner of that org and
  is not a platform admin. This is what an agent working in one org should
  hold: `isb token create agent --org acme`, then point it at
  `/orgs/acme/mcp` ([Agents and MCP](../guides/agents.md)).
- **platform** (no org): the user's whole reach. Platform admins only; it
  stops working if the user stops being one.

Tokens can expire (`--expires 90d`, `"expires": "90d"`) or not. Each records
when it was last used. Users list and revoke their own; an org's owners and
admins list and revoke any token in it.

**A token cannot mint tokens.** New tokens come from a signed-in browser
session, an Access or tailnet identity, or `isb token create` on the host;
an API token, a workspace token and a superadmin token are all refused
(`POST tokens` and the `token_create` tool alike). A token that could mint
another would survive its own revocation through the copy, and bounding the
copy's scopes or expiry would not change that: revoking a leaked token must
end what it can do. For the same reason no token creates a user, sets a
user's password or adds an SSH key to someone else's account.

### Scopes

A token with no scopes has its user's whole role: an agent in an org
administers the org's apps, as its people do. Scopes are an opt-in
narrowing, checked in the same authorizer as roles, and never widen a role
(a viewer's `admin` token still only reads):

| Scope | Allows |
|---|---|
| `read` | read-only tools, never secret material (as a viewer) |
| `deploy` | `read`, plus `stack_deploy`, `stack_redeploy`, `stack_rollback`, `stack_scale`, `app_scale`, `app_restart`, `instance_restart`, `sandbox_start`, `sandbox_stop`, `app_deploy`, `app_rollback`, `build_run` |
| `admin` | everything the role allows (the same as no scopes) |
| `tool:GLOB` | tools whose name matches, e.g. `tool:app_*`, `tool:stack_status` |

A token may hold several; a call passes if any allows it. A scoped token
without `admin` cannot change accounts, tokens, invitations, members or SSH
keys (every state-changing `/api/v1/auth/*` request, and every account tool
that changes something, is refused), and tokens
whose scopes leave out `sandbox_exec` (`read`, `deploy`) get no terminal or
SSH. Give scopes with `"scopes": ["deploy", "tool:app_*"]` on `POST tokens`,
`isb token create --scope deploy --scope 'tool:app_*'`, or the web UI's
**Access** choice (full, deploy, read only, or only some tools).

A request carrying `Authorization` is judged by it alone: a bad token is a
401 even with a valid session cookie.

## What each caller may reach

- `overview`, `events` and `stack_list` show only the caller's orgs.
- `org_get` is for the org's members.
- `audit_list` shows an org's owners and admins their org's entries and
  platform admins everything; `audit_verify` is for platform admins.
- The `server_*` tools are for platform admins. A call for an org placed on
  a server is judged on the control plane, then again by the server's agent
  ([Placement](placement.md#how-the-control-plane-works)).
- The unix socket is the daemon's own user and reaches everything; so does a
  superadmin. `host_inventory`, `host_monitor`, `host_policy`, `superadmin_token_list`,
  `superadmin_token_revoke` and `superadmin_list` are for superadmins only.
- `--allow-tools` and `--deny-tools` (names or globs, deny wins) choose which
  tools remote callers see at all; the local socket always has every tool
  ([Security model](security.md#tool-policy)).
- Every call that changes something, every refusal, every secret read,
  sign-in, webhook delivery and terminal session is recorded in the
  [audit log](../operations/audit.md), and every controller event and incus
  lifecycle event in the [history](../operations/history.md).
