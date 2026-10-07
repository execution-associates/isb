---
title: Notifications
description: Tell an org's people about failed deploys, unhealthy services, certificates, backups and jobs, through webhooks, Slack, Discord, Telegram or email.
order: 9
---

You want to hear about a failed deploy or a service that lost every healthy
replica before your users do. `isb serve` tells an org's people about what
happens to its apps and stacks: deployments that succeed or fail, services
that lose every healthy replica and recover, certificates issued or failing,
and backups and scheduled jobs. Each org has its own **channels**; a channel
is a destination plus **rules** choosing which events it hears about. Only
the org's own events ever reach its channels.

```sh
isb secret create OPS_HOOK <<<'https://hooks.example.com/isb'   # the URL is a secret
isb secret create OPS_HOOK_KEY <<<"$(openssl rand -hex 32)"
isb notify create ops --webhook OPS_HOOK --signing-secret OPS_HOOK_KEY --events 'deploy.*,health.*'
isb notify test ops            # sends a test message now, prints the outcome
isb notify deliveries ops      # what was sent, retried, failed
```

```text
isb notify create NAME (--webhook URL_SECRET [--signing-secret S] | --slack URL_SECRET | --discord URL_SECRET
               | --telegram TOKEN_SECRET --chat-id ID | --smtp-host H [--smtp-port P] [--smtp-tls starttls|tls|none]
               [--smtp-user U --smtp-password-secret S] --from ADDR --to ADDR...)
               [--events deploy.*,health.*] [--app-project P]... [--app A]... [--stack S]... [--disabled]
isb notify ls [--json] | show NAME | rm NAME
isb notify update NAME [--events ...] [--app-project P]... [--app A]... [--stack S]... [--enable|--disable]
isb notify test NAME                       sends a test message now; exit 1 if it failed
isb notify deliveries NAME [-n 20] [--json]
isb notify settings [--allow-private-targets true|false]   platform admins
```

All commands take `--org ORG`. The same operations are tools on MCP and REST
([below](#tools)), so the web UI's **Notifications** section and agents use
them too.

## Destinations

Every URL, token and password lives in the org's secret store
([Secrets](secrets.md)) and a channel names the secret, never the value. A
secret's value is read at send time, so `isb secret set` rotates it without
touching the channel.

| Type | Settings | Sends |
|---|---|---|
| `webhook` | `url_secret`, optional `signing_secret` | A JSON `POST` ([below](#the-webhook-body)) to any http(s) URL. |
| `slack` | `url_secret`: an incoming-webhook URL, `https://hooks.slack.com/services/...` | `{"text": ...}`, the title in bold. |
| `discord` | `url_secret`: `https://discord.com/api/webhooks/...` | `{"content": ..., "allowed_mentions": {"parse": []}}` (never pings). |
| `telegram` | `token_secret` (a bot token), `chat_id` (`-100...` or `@channel`) | `sendMessage` to the Bot API, plain text. |
| `email` | `host`, `port` (default by `tls`), `tls`: `starttls` (587, default), `tls` (465) or `none` (25); `username` + `password_secret`; `from`; `to` (1 to 20) | A plain-text mail through that SMTP server. A password is never sent without TLS. |

Slack and Discord URLs are checked against their hosts when the channel is
saved and again at every send; a Telegram token must look like one
(`123456:ABC...`), so it cannot change the request path.

### The webhook body

```json
{
  "id": "1791014653123-7",
  "org": "acme",
  "kind": "deploy.failed",
  "level": "error",
  "stack": "shop-production",
  "service": "web",
  "project": "shop",
  "message": "app web: deployment 3 failed: build: ...",
  "at": 1791014653123,
  "seq": 412,
  "test": false
}
```

Headers: `Content-Type: application/json`, `X-Isb-Event: <kind>`,
`X-Isb-Delivery: <id>` (the same for every retry of one delivery: receivers
deduplicate on it), and with a signing secret
`X-Isb-Signature: sha256=<hex HMAC-SHA256 of the raw body, keyed by the
secret>`. To verify, compute the HMAC over the bytes received and compare in
constant time:

```python
import hmac, hashlib
ok = hmac.compare_digest(sig, "sha256=" + hmac.new(key, body, hashlib.sha256).hexdigest())
```

`at` (unix milliseconds) is inside the signed body, so a receiver can refuse
old replays. Events whose producer adds structure carry it in `details`:
`monitor.*` events have the URL, HTTP status, latency, error, downtime and a
link to the monitor ([Uptime monitoring](uptime.md#the-notification)); Slack,
Discord, Telegram and email show the latency and the link too.

## Rules

A channel's `rules` is a list; an event is sent when **any** rule matches. A
rule matches when the event's kind matches one of its `events` globs (`*`
matches any run of characters) and it passes every filter given:

| Field | Matches |
|---|---|
| `events` | `deploy.*`, `health.*`, `monitor.*`, `backup.*`, `job.*`, `cert.*`, `secret.*`, `*.failed`, `*` (the default) |
| `projects` | the app's project (only app events have one) |
| `apps` | the app (a service of that name) |
| `stacks` | the stack's name in the org (`<project>-<env>` for apps) |

With no `rules`, a channel hears every event that has a kind. `isb notify
create` builds one rule from `--events`, `--app-project`, `--app` and
`--stack`; the `notification_channel_*` tools take the full list. The web
UI's channel dialog ticks event kinds by subject and writes them back as the
shortest globs (`*`, `deploy.*`, `*.failed`).

## Event kinds

| Kind | When |
|---|---|
| `deploy.succeeded`, `deploy.failed` | An app deployment finished (build, push and rollout), or failed at any step. Preview deployments too. |
| `health.unhealthy` | A service that was healthy has had no healthy replica (none in rotation) for 20 s. Not raised during a rollout (a failed rollout is `deploy.failed`), nor before the service was first healthy in this daemon run. |
| `health.recovered` | A replica of that service is healthy again. |
| `cert.issued`, `cert.failed` | The ingress got a certificate for a domain, or failed to ([Domains and ingress](domains.md)). |
| `backup.succeeded`, `backup.failed`, `job.succeeded`, `job.failed` | [Database and volume backups](databases.md#backups) and [scheduled jobs](jobs.md). |
| `monitor.down`, `monitor.up` | An [uptime monitor](uptime.md) went down (its failure threshold reached), or came back (with the downtime). Once per incident; a flapping monitor is held until it settles. The body's `details` carry the URL, status, latency, error and a link. |
| `monitor.cert_expiring` | An HTTPS certificate a monitor sees expires within its `cert_expiry_days`; once per certificate. |
| `secret.rotated` | A new secret version reached a stack service (or an app): the message says the versions and what its `on_change` does (rolling, restarting in place, or not cycled, at `warn`). A workspace using it is reported under stack `<org>/@workspaces`. See [When a secret changes](secrets.md#when-a-secret-changes). |

The list is the `kind` field on the event feed (the `events` tool, the SSE
stream); a producer adding a kind adds it there, and a `*` rule hears it.

## Delivery

- A dispatcher follows the daemon's event feed. The feed lives in memory and
  starts over with each daemon run, so the dispatcher starts from the
  beginning of the run: an event is never sent twice across a restart.
  Deliveries still queued when the daemon stops are lost.
- Each channel has its own queue (100 deliveries; the oldest is dropped, and
  logged as `dropped`, past that) and its own sender, so a slow or broken
  destination never delays another channel.
- A failed attempt is retried when it may succeed later (connection errors,
  timeouts, HTTP 408, 429 and 5xx, SMTP 4xx), up to 6 attempts, waiting 5 s,
  10 s, 20 s, 40 s and 80 s, or longer when the server sends `Retry-After`
  (at most 5 min). Other answers (4xx, a refused address, a missing secret)
  fail at once.
- A channel sends at most 20 messages a minute; more wait their turn.
- `notification_deliveries` keeps each channel's last 50 deliveries (in
  `<state>/orgs/<org>/notify/deliveries/`), with status `queued`, `retrying`,
  `sent`, `failed`, `dropped` or `skipped` (the channel was disabled or
  removed meanwhile), attempts, the HTTP status and the error. Errors name
  the host, never the URL or a token.
- `notification_test` sends one test message (`kind: "test"`) at once,
  without retries, and returns its outcome; it is logged too.

## Private destinations (SSRF)

A channel's destination is chosen by an org member, so it must not become a
way into the host's network. Every destination (webhook URL, SMTP host, and
the fixed Slack, Discord and Telegram hosts too) is resolved by the daemon,
**every** address it resolves to is checked, and the connection goes to one
of those checked addresses, never re-resolved, so DNS rebinding cannot swap
one in; the connected peer is checked again. Redirects are never followed (a
3xx is a failure). URLs must be `http://` or `https://`, without credentials.

Refused by default: loopback (`127.0.0.0/8`, `::1`), private ranges
(`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), link-local (`169.254/16`,
`fe80::/10`, which holds cloud metadata services), shared address space
(`100.64/10`, which holds tailnets), benchmarking, documentation and reserved
ranges, and IPv6 forms that carry such an IPv4 address (`::ffff:a.b.c.d`,
`::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`; Teredo too). Numeric
host spellings (`2130706433`, `0x7f000001`, `0177.0.0.1`, `127.1`) are read
as the address they name and refused the same way. Unspecified, multicast and
broadcast addresses are never reachable.

A platform admin can allow private targets for the whole server, for a
receiver on the host or the LAN:

```sh
isb notify settings --allow-private-targets true    # or the notification_settings tool
```

It is stored in `<state>/notify.json` and applies at once. The same policy,
never relaxed, guards template logo fetches
([Templates](templates.md#logos)).

## Tools

| Tool | Does |
|---|---|
| `notification_channel_create` | Add a channel (`name`, `provider`, `rules`, `enabled`); its secrets must exist. |
| `notification_channel_list` | The org's channels, with each one's last delivery. |
| `notification_channel_get` | One channel. |
| `notification_channel_update` | Replace a channel's `provider`, `rules` or `enabled`; fields left out are kept. |
| `notification_channel_delete` | Remove a channel and its delivery log (its secrets stay). |
| `notification_test` | Send a test message now; returns the delivery. |
| `notification_deliveries` | A channel's recent deliveries, newest first. |
| `notification_settings` | `allow_private_targets`, server-wide. Platform admins. |

The channel tools act in one org: its members may use them (viewers only the
read-only ones, as everywhere), nobody else ([Users, roles and superadmins](../concepts/access.md)). A `provider` is
written as an object with a `type`:

```json
{"name": "oncall", "provider": {"type": "telegram", "token_secret": "TG_BOT", "chat_id": "-1001234567890"},
 "rules": [{"events": ["deploy.failed", "health.*"], "projects": ["shop"]}]}
```
