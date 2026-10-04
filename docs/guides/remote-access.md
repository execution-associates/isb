---
title: Reach isb serve remotely
nav_title: Remote access
description: Put the daemon's web UI, MCP server and API behind Cloudflare Tunnel and Access, or serve it on a tailnet.
order: 12
---

`isb serve` never listens on an open port. It answers on a unix socket for
the host's own user and, with `--listen`, on loopback HTTP. To reach the web
UI, the MCP server and the REST API from anywhere else, put something in front
of loopback that decides who gets through: a Cloudflare Tunnel with Access
(SSO in front of everything, no inbound port), or your Tailscale network.
This page sets up both.

## What `--listen` accepts

`--listen` (`ISB_SERVE_LISTEN`) takes one or more addresses, comma-separated,
and refuses anything but loopback or a tailnet address (100.64.0.0/10,
fd7a:115c:a1e0::/48). Put a tunnel (or a reverse proxy) in front of loopback,
never an open port. Cloudflare Access
guards the loopback listeners; a tailnet listener is reached only by tailnet
peers and has no Access in front.

Whatever is in front, every HTTP caller still signs in as an isb user (a
session, an API token), is a superadmin, or is an agent identity an org
mapped; Access and the tailnet decide who
reaches the port, isb decides what they may do ([Users, roles and
superadmins](../concepts/access.md)).

## Cloudflare Tunnel and Access

The pattern is the one [herdr-mcp](https://github.com/Orange-County-AI/herdr-mcp)
uses: Cloudflare runs the OAuth flow, isb stays the resource origin and checks
the identity Cloudflare signs. Remote MCP clients such as Claude and ChatGPT
connect with nothing but the URL; no secret is copied into them.

### 1. Serve on loopback

```dotenv
# ~/.config/isb/serve.env
ISB_SERVE_LISTEN=127.0.0.1:8092
CF_ACCESS_TEAM_DOMAIN=https://your-team.cloudflareaccess.com
CF_ACCESS_AUD=your-access-application-audience-tag
ISB_PUBLIC_URL=https://isb.example.com
```

`isb serve install` writes `ISB_SERVE_LISTEN=127.0.0.1:8092` into a new
`serve.env` by default; add the rest and `systemctl --user restart isb`.
Access is the front door; it does not replace
isb's own sign-in. Without the `CF_ACCESS_*` values the listener still serves
the web UI, the identity endpoints and the tools to callers with an isb
session or API token, and refuses anonymous calls (unless
`--allow-unauthenticated`, which is for local testing only). With them, a
request must also carry a valid Access assertion. `ISB_PUBLIC_URL` is what
invitation links, provider sign-in and passkeys are built from
([Sign-in](sign-in.md)).

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

With both values set, every `/mcp` request must carry `Cf-Access-Jwt-Assertion`,
and so does every other request on the loopback listeners: the web UI, the
REST tools and `/api/v1/auth/*`, where isb's own sign-in then applies behind
Access. isb fetches the team's signing keys from
`<team>/cdn-cgi/access/certs` (cached for an hour, refetched for an unknown
key id, rate-limited), and verifies the RS256 signature, the issuer, the
audience and the expiry. A request that reaches the port by any other route
is refused. Do not put another OAuth server behind Access Managed OAuth:
Access replaces the origin's 401 handling by design.

An Access identity whose email belongs to an isb user acts as that user; one
that does not is told to ask an org admin for an invitation.

**Webhooks.** Forges and registries cannot sign in to Access, so add an Access
**bypass** policy for `/api/v1/webhooks/*`. The daemon serves that path ahead
of its Access check, since each request is signed with the app's webhook
secret ([Deploy apps](deploy-apps.md#webhooks)).

### 4. Connect a client

The MCP URL is `https://isb.example.com/mcp`, or
`https://isb.example.com/orgs/ORG/mcp` for one org. Claude adds it as a remote
(Streamable HTTP) MCP server; ChatGPT as a custom connector. The first
connection opens the Access login. Agents that cannot do a browser login use
an isb API token plus an Access service token's headers
(`CF-Access-Client-Id`, `CF-Access-Client-Secret`); the web UI's MCP page
writes those snippets ([Agents and MCP](agents.md)).

Every call is logged to the daemon's stderr (the journal) with the caller's
identity, the tool, its duration and whether it failed, never its arguments,
and calls that change something are in the [audit log](../operations/audit.md).
Stacks record who deployed them (`deployed_by`).

### 5. Agents and superadmins through Access (optional)

An org can map an Access service token's client id, or the email of someone
who is not an isb user, to a role in that org only ([Agent
identities](../concepts/access.md#agent-identities)); the agent sends the
service token's `CF-Access-Client-Id` and `CF-Access-Client-Secret`. The same
CSRF, `Origin`, `Content-Type` and `Host` checks apply (the `Host` against the
public URL when `--public-url` is set).

#### Superadmins

`--superadmin-access alice@example.com,abc123.access` (or
`ISB_SUPERADMIN_ACCESS`) gives the listed Access identities, users by email
and service tokens by client id, the unix socket's reach. Only a verified
assertion counts, so the flag is refused without both `CF_ACCESS_*` values,
and it needs `--public-url` (an Access superadmin's `Host` must be it). The
`CF_Authorization` cookie makes it ambient, so the same CSRF, `Origin`,
`Content-Type` and `Host` checks as for tailnet superadmins apply
([Superadmins](../concepts/access.md#superadmins)); an MCP client that sends a
foreign `Origin` is refused as a superadmin. Emails match case-insensitively
and exactly, service tokens by client id, with no wildcards or domains.

## A tailnet as the trust boundary

To give people and agents on a Tailscale network superadmin access:

```dotenv
# ~/.config/isb/serve.env
ISB_SERVE_LISTEN=127.0.0.1:8092,100.86.22.100:8092
ISB_SUPERADMIN_TAILNET=someone@example.com,tag:agents
```

The tailnet address is this host's own (`tailscale ip -4`). A request from a
tailnet peer is a superadmin when tailscaled says its user's login, or one of
its node's tags, is on the list (a tagged node only by its tags). Everyone
else on that listener signs in as on any other (tokens, sessions). The
`Host` a browser sends must be the listen address, the node's MagicDNS name
(`host` or `host.tailnet.ts.net`) or the public URL's host. An empty list is
refused.

The identity comes only from the TCP peer address, asked of the local
tailscaled (`whois` over its LocalAPI socket, else the `tailscale` CLI on
macOS), cached for a minute per address. Forwarded headers
(`X-Forwarded-For`, `Tailscale-User-Login`) are never read. When tailscaled
does not answer, nobody is a tailnet superadmin, and the daemon says why
once.

A superadmin is root on the host in all but name: grant it to the people and
tagged machines that administer the host, nobody else. For agents that should
reach one org, give them an org API token, or map their tailnet login or tag
to a role in the org ([Agent identities](../concepts/access.md#agent-identities)):

```dotenv
# ~/.config/isb/serve.env: a tailnet listener is enough for org agent identities
ISB_SERVE_LISTEN=127.0.0.1:8092,100.86.22.100:8092
```

The org's owners and admins then add `tag:agents` (or a login) with a role in
Settings, Agent identities, and an agent on a node with that tag connects to
`http://100.86.22.100:8092/orgs/acme/mcp` with no token. The identity is asked
of tailscaled exactly as for superadmins; the `Host`, CSRF, `Origin` and
`Content-Type` checks apply, and it reaches that org only.

## Checking it

```sh
journalctl --user -u isb | grep -i superadmin   # which superadmin sources are on, at start-up
curl -s http://127.0.0.1:8092/healthz           # answers without auth
```

A superadmin can read the whole serving policy (listen addresses, Access,
the tool policy, what remote specs may ask for, each superadmin source) with
the `host_policy` tool or the web UI's **Host** page. Every flag is in
[Configuration](../reference/configuration.md#daemon-flags).
