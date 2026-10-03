---
title: The audit log
description: Who did what, through which door, and how it went, kept append-only with a hash chain that shows tampering.
order: 4
---

When an app was redeployed at 3 a.m., a secret read, or a member removed, you
want to know who did it and how they got in. `isb serve` records every call
that changes something, every sign-in and account change, every secret read,
webhook delivery and terminal or SSH session. It never records a secret value
or a tool's arguments as a whole.

The audit rows are also one of the four sources of the
[history](history.md), where they sit next to what the controller and incus
did.

## What is recorded

| Kind | Action | When |
|---|---|---|
| Tool calls | the tool's name (`stack_deploy`, `secret_set`, ...) | every call to a tool that is not read-only, on every surface (MCP, REST, the web UI, the CLI over the unix socket); and every refusal (`forbidden`), read-only or not |
| Secret reads | `secret_get`, `secret_resolve`, `app_webhook`, and `database_get` with `reveal` | always: they hand out secret material |
| Reads | the tool's name | only with `--audit-all` (for compliance) |
| Superadmins | the tool's name; `superadmin.refused` | every call a superadmin makes over HTTP, reads included; and every superadmin request refused by the CSRF, `Origin`, `Content-Type` or `Host` checks ([Access](../concepts/access.md#superadmins)) |
| Sign-in | `auth.setup`, `auth.login` (password, passkey, provider), `auth.logout` | success and failure; a failure names the address that was tried |
| Accounts | `auth.password_change`, `auth.password_reset_request`, `auth.password_reset`, `auth.passkey_add`, `auth.passkey_remove`, `auth.identity_link`, `auth.identity_unlink`, `auth.session_revoke` | |
| Tokens | `auth.token_create`, `auth.token_revoke`, `auth.superadmin_token_create`, `auth.superadmin_token_revoke` | from the web UI, the API and `isb token` |
| Orgs | `auth.invitation_create`, `auth.invitation_accept`, `auth.invitation_revoke`, `auth.role_change`, `auth.member_remove` | |
| Users | `auth.user_create`, `auth.user_disable`, `auth.user_enable`, `auth.platform_admin_grant`, `auth.platform_admin_revoke` | |
| Webhooks | `webhook.deploy` | every delivery to `/api/v1/webhooks/<org>/<app>`: `ok` (deployed), `ignored` (another branch, a ping), `unauthorized` |
| Terminals | `terminal.open`, `terminal.close` | with the app and replica (or the instance) and (on close) how long it ran; never what was typed |
| SSH | `ssh.open`, `ssh.close`, `auth.ssh_key_add`, `auth.ssh_key_remove` | a session with its instance, and on close the guest user, the key's fingerprint and how long it ran; a key change with its fingerprint ([SSH](../guides/ssh.md)) |

Recording happens where every call passes (the dispatch hook in front of the
tool registry, the identity endpoints' router), so a new tool is recorded
without doing anything. A tool that hands out secret material and is
annotated read-only must say so, with `"isbSecretRead": true` in its
annotations (or by being listed in `SECRET_READS`, in
`crates/isb-daemon/src/daemon/audit.rs`): it is then always recorded and
refused to viewers and to `read`/`deploy` tokens. A tool that hands out
secrets only when asked names the boolean argument that asks
(`"isbSecretReadArg": "reveal"` on `database_get`): calls that set it are
secret reads, the rest are ordinary reads.

## An entry

| Field | |
|---|---|
| `id` | position in the log, from 1, never reused |
| `time` | unix milliseconds |
| `org` | the org the call acted in; `null` for platform-level entries (sign-ins, accounts, users, org-spanning tools) |
| `actor` | the user's email; `local(uid N)` for the unix socket and the host CLI; `access:EMAIL` for a Cloudflare Access identity; `webhook:PROVIDER` (`github`, `gitlab`, `gitea`, `token`); `workspace` for an org's workspace token; a superadmin's source (`token:<name>`, `tailnet:<login>`, `access:<name>`) |
| `actor_kind` | `person` (a session, or Access mapped to a user), `agent` (an API token or a workspace token), `local`, `webhook`, `anonymous` (a failed sign-in, an Access identity without an account), `superadmin` |
| `user_id`, `user_email`, `token_id`, `token_name` | who, when known; a workspace's `token_name` is `workspace:<name>` |
| `surface` | `mcp`, `rest`, `web` (a browser session), `cli` (the unix socket or the host CLI), `webhook` |
| `action` | as above |
| `target` | what it acted on: the first of the arguments `name`, `app`, `stack`, `project`, `service`, `environment`, `id`, `user_id`, `email`; for the `org_*` tools, the org |
| `details` | whitelisted arguments only: names and identifiers (`org`, `name`, `app`, `service`, `replicas`, `version`, `role`, `slot`, ...), each a scalar of at most 128 characters. `value`, `password`, `token`, `env`, `vars`, `secrets`, `compose`, `argv`, `stdin`, URLs and anything else are dropped |
| `outcome` | `ok`, or the error's code (`forbidden`, `not_found`, `invalid`, `invalid_credentials`, ...). Never the error's message, which can quote arguments |
| `ip`, `user_agent`, `request_id` | when known: the client address (`Cf-Connecting-IP` behind the tunnel), its agent, and `X-Request-Id` (when sane), `Cf-Ray`, or a fresh id |
| `prev_hash`, `hash` | the chain (below) |

## Storage, retention, tamper evidence

- SQLite at `<state>/audit.db`, its own file (0600 in a 0700 directory), so
  it keeps its own retention and schema. The daemon and the host CLI both
  append to it.
- **Append only.** There is no update or delete API, and triggers refuse
  `UPDATE` and any `DELETE` that is not retention pruning.
- **Retention**: entries older than `--audit-retention` (default `90d`) are
  pruned at startup and hourly. Pruning is the only way an entry leaves.
- **Hash chain**: each entry's `hash` is SHA-256 over its `prev_hash` and
  every other field (keys sorted), and `prev_hash` is the hash of the entry
  before it. Editing, removing or reordering an entry breaks the chain from
  there on, and removing the newest ones no longer matches the head kept
  beside them. Pruning keeps the hash of the last pruned entry, so the chain
  still starts somewhere known.
- `audit_verify` (platform admins) and `isb audit verify` walk the chain and
  report `{ok, rows, head: [id, hash], pruned_through, broken: [id, why]}`.
  Anyone who can write the file can rebuild a whole chain; copy the head
  somewhere else (a ticket, another host) to pin the log up to it.
- On a control plane the audit log is the control plane's: it records every
  call before forwarding it to a server, and each server's agent keeps its
  own log as well ([Servers](../guides/servers.md)).

## Who reads it

| Caller | Sees |
|---|---|
| platform admin, the unix socket, the host CLI | everything, platform-level entries included |
| org owner or admin | that org's entries |
| member, viewer | nothing (refused) |
| API token | as its user's role in its org, when its scopes cover `audit_list` (none, `read`, `admin`, or a matching `tool:` glob) |

Reading the log is itself a read, so it is not recorded unless `--audit-all`.

## The API

- `audit_list` (a tool, so also `POST /api/v1/tools/audit_list` and
  `/orgs/<org>/api/v1/tools/audit_list`): filters `org`, `platform` (only
  platform-level entries), `actor` (glob on the actor or the email),
  `user_id`, `token_id`, `action` (glob: `secret_*`, `auth.*`), `target`
  (glob), `outcome` (`ok`, `error` for anything else, or a code), `surface`,
  `since`/`until` (unix ms), `limit` (default 100, at most 1000). Newest
  first; page with `before` = the `next_before` of the last page. With
  `after` instead, the entries newer than that id, oldest first (tailing).
  Returns `{entries, next_before, head}`.
- `GET /api/v1/audit/stream[?org=ORG][&after=ID]`: server-sent events
  (`event: audit`, the entry as `data`, its id as `id`), from `after` (or
  `Last-Event-ID`, or now) on; only entries the caller may read. Signed in
  with a session or a token. Keepalive comments every 15 s.
- `audit_verify`: the chain check above.

## The CLI

On the host, as the daemon's user, on `<state>/audit.db` directly (like
`isb user` and `isb token`, so it works while the daemon is down):

```text
isb audit ls [--org ORG | --platform] [--actor GLOB] [--action GLOB]
             [--target GLOB] [--outcome ok|error|CODE] [--since 24h] [--until 1h]
             [-n 50] [--json]
isb audit export [same filters]      every match as JSON lines, oldest first
isb audit verify                     both chains (audit and history); exits 1 when an entry does not check out
```

For example, every secret read in one org over the last day:

```sh
isb audit ls --org acme --action 'secret_*' --since 24h
```

## The web UI

The audit rows are one source of the **History** page
([history](history.md#the-web-ui)), under each org and in **Platform**;
`/orgs/ORG/audit` opens it filtered to the audit log. Audit rows show there
for org owners and admins and platform admins only.

## Settings

| Flag | Environment | Default |
|---|---|---|
| `--audit-retention` | `ISB_AUDIT_RETENTION` | `90d` |
| `--audit-all` | `ISB_AUDIT_ALL` | off: read-only tool calls are not recorded (secret reads and refusals always are) |
