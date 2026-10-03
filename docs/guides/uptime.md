---
title: Uptime monitoring
description: Hear about it when an app users reach is down, with uptime history, incidents, certificate expiry warnings, and a heartbeat for when the host itself dies.
order: 9.5
---

You want to know when one of your apps is down before your users tell you.
Health checks inside a stack say whether a replica answers; they cannot see a
broken DNS record, an expired certificate, a tunnel that stopped, or an
ingress that routes nowhere. An **uptime monitor** checks what users see,
from outside the app, every interval, and tells your
[notification channels](notifications.md) once when it goes down and once
when it comes back.

Every app with a served domain gets a monitor of its own (`app-<name>`)
within a minute, so with a channel that hears `monitor.*` there is nothing
else to set up:

```sh
isb secret create OPS_HOOK <<<'https://hooks.slack.com/services/...'
isb notify create ops --slack OPS_HOOK --events 'monitor.*,health.*'
```

The web UI's **Uptime** section (under Org) lists every monitor with its
status, 24 hours of uptime bars, a latency sparkline and recent incidents;
each app's **Monitoring** tab shows the monitors watching it, and the org's
overview shows a banner while any monitor is down.

## What a monitor checks

| Type | Checks | Up when |
|---|---|---|
| `app` | an app, by name: its served domain's public URL (the first, or `domain`), with `path` | the answer's status is in `expected_status` and the keywords hold |
| `http` | any `url` (`http://` or `https://`) | the same |
| `tcp` | `host` and `port` | a connection opens |

An `app` monitor follows the app: rename its domain, add one, scale it, and
the monitor checks what is served now. With no domain it checks the app's own
endpoint instead: its published port, else a replica in rotation (and with no
replica in rotation it is down, saying so).

HTTP checks take:

| Field | Default | |
|---|---|---|
| `method` | `GET` | or `HEAD` |
| `expected_status` | `200-399` | codes and ranges: `200`, `200,204`, `200-299,301` |
| `keyword`, `keyword_absent` | none | text the body (its first 256 KiB) must, or must not, contain |
| `follow_redirects` | `false` | follow up to 5; otherwise a 3xx is judged as it is |
| `headers` | none | `[{name, value}]` or `[{name, secret}]`: a secret's value is read at each check and never shown |
| `cert_expiry_days` | `14` | `monitor.cert_expiring` this many days before an HTTPS certificate expires, once per certificate; `0` never |

And every monitor:

| Field | Default | |
|---|---|---|
| `interval` | `60` | seconds between checks, at least 30 |
| `timeout` | `10` | seconds a check may take (connect, TLS, answer), under the interval |
| `failure_threshold` | `2` | failed checks in a row that make it down |
| `recovery_threshold` | `2` | successful checks in a row that make it up again |
| `paused` | `false` | a paused monitor is not checked; its history stays |

```sh
# Over REST (or MCP): any URL, with a keyword.
curl -H "Authorization: Bearer $ISB_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "docs", "type": "http", "url": "https://docs.example.com/", "keyword": "Welcome"}' \
  https://isb.example.com/orgs/acme/api/v1/tools/monitor_create
```

## Down, up, and nothing in between

- `failure_threshold` failures in a row make a monitor **down**: channels get
  `monitor.down` once, and an incident opens, dated from the first failure.
- `recovery_threshold` successes in a row make it **up**: channels get
  `monitor.up` with how long it was down, and the incident closes. One
  success between failures does not count (hysteresis).
- Notifications alternate: never two downs or two ups in a row, and an up only
  after a down was sent. A new monitor's first success is not news; a new
  monitor that is down is.
- A monitor that went down 3 times within 30 minutes is **flapping**: the down
  that makes it so is sent (saying so), then nothing until it has held one
  state for 30 minutes; then the state it settled in is sent, if channels last
  heard otherwise. Incidents are still recorded meanwhile.
- State lives in the history database, so a daemon restart sends nothing
  twice.

## The notification

`monitor.down`, `monitor.up` and `monitor.cert_expiring` are event kinds like
any other ([Event kinds](notifications.md#event-kinds)). An `app` monitor's
events are about the app's service (so a rule's `apps` and `projects` filters
match them); other monitors' are about the stack `@monitors`, service = the
monitor. The webhook body carries the details:

```json
{
  "id": "1791068703714-2",
  "org": "acme",
  "kind": "monitor.down",
  "level": "error",
  "stack": "shop-production",
  "service": "web",
  "project": "shop",
  "message": "Monitor app-web is DOWN: https://shop.example.com/: HTTP 503 (expected 200-399) (2 failed checks in a row)",
  "details": {
    "monitor": "app-web",
    "type": "app",
    "app": "web",
    "url": "https://shop.example.com/",
    "status": 503,
    "latency_ms": 32,
    "error": "HTTP 503 (expected 200-399)",
    "via": "public",
    "checked_at": 1791068703620,
    "down_since": 1791068672596,
    "failures": 2,
    "flapping": false,
    "link": "https://isb.example.com/orgs/acme/uptime/app-web"
  },
  "at": 1791068703655,
  "seq": 20,
  "test": false
}
```

`monitor.up` adds `downtime_ms` and `downtime` (`1m 31s`);
`monitor.cert_expiring` adds `cert_expires_at` (unix seconds) and
`cert_days_left`. Slack, Discord, Telegram and email get the message with the
latency and the link. The link needs `--public-url`. URLs in messages and
details never carry their query string, which may hold a token.

## Apps' own monitors

Every app with a served domain gets `app-<name>`: GET its first domain,
`200-399`, every 60 s, down after 2 failures. Edit it like any other monitor
(it stays the app's). It goes away when the app does, or loses its domains.
Deleting it adds the app to the org's exclusions so it does not come back;
`monitor_settings` turns the whole thing off (`auto_monitors: false`) or edits
the exclusions (`exclude_apps`).

### Behind Cloudflare Access

A domain behind [Cloudflare Access](remote-access.md) answers a sign-in
redirect, not the app. isb recognises the redirect (to
`*.cloudflareaccess.com`) and never counts it as up:

- **Give monitors a service token** (recommended): create an Access service
  token allowed by the application's policy, and store it as the org secrets
  `CF_ACCESS_CLIENT_ID` and `CF_ACCESS_CLIENT_SECRET`. Every `app` monitor
  that sets no Access headers of its own presents them, so the check goes
  through Access to the app, as users do. Any monitor can also name them in
  `headers` (`CF-Access-Client-Id`, `CF-Access-Client-Secret`).
- **Without a token**, an `app` monitor checks the app's own endpoint instead
  and says so in its last check (`via`, `note`); an `http` monitor is down
  with "redirected to Cloudflare Access sign-in", since that is all it can
  see.

## Where checks run, and what they may reach

Checks run in the daemon that runs the org's apps: `isb serve` on a single
host; for an org placed on a [server](servers.md), that server's agent (the
`monitor_*` tools follow the org there like every other org tool, and so do
its monitors, history and channels). The agent sees the app's real URL from
the network the app lives on, and keeps checking and notifying if the control
plane is down. What the agent cannot report is its own death: the control
plane raises `server.unreachable` when a server stops answering its heartbeat
(3 misses in a row, 10 s apart) and `server.recovered` when it answers again, and a
[heartbeat](#host-down-a-dead-mans-switch) covers a host that dies outright.

- A scheduler hands due checks to 8 workers through a bounded queue, so slow
  targets never pile up threads and a check never overlaps itself. First
  checks are spread by name within a minute; then every interval plus a
  little jitter.
- URLs and hosts a member types are held to the platform's address policy,
  the one [notification channels](notifications.md#private-destinations-ssrf)
  use: every address a name resolves to must be public, the connection goes
  to a checked address, and redirects are checked again. A platform admin's
  `notification_settings` `allow_private_targets` relaxes it for monitors
  too.
- An app's own endpoint (a replica, a published port) is found by reference,
  not typed, so it is reached directly whatever the policy. An `app`
  monitor whose domain resolves to a private address while private targets
  are refused checks that endpoint instead, and says so.

## History

Per org, in `<state>/orgs/<org>/monitors/monitors.db` (SQLite): every check
for 7 days, hourly rollups (checks, successes, latency p50 and p95) for 90
days, incidents for 90 days after they end. A monitor checked every 30 s keeps
about 20,000 rows. `monitor_list` and `monitor_get` report uptime over 24 h,
7 d and 30 d and latency p50/p95 over 24 h; `monitor_checks` returns buckets
over `1h`, `24h`, `7d`, `30d` or `90d` and the latest raw checks. Definitions
are `monitors.json` and `settings.json` beside it.

## Host down: a dead man's switch

No event from the daemon can say the host died. Point `isb serve` at an
outside "ping" check (healthchecks.io, Better Stack, Cronitor, an Uptime Kuma
push monitor) and it GETs the URL every interval; when the pings stop, that
service alerts you.

```sh
# The URL's path is the check's token: keep it out of argv and logs.
ISB_HEARTBEAT_URL=https://hc-ping.com/<uuid> isb serve ...
```

| Flag | Environment | Default | |
|---|---|---|---|
| `--heartbeat-url` | `ISB_HEARTBEAT_URL` | off | the URL to GET; logs name only its host |
| `--heartbeat-interval` | `ISB_HEARTBEAT_INTERVAL` | `60s` | 10 s to 1 h; set the outside check's period a little longer |

A 2xx or 3xx answer counts. The daemon logs when the heartbeat starts and
stops working. Agents take the same flags, so each server can have its own
check.

## Tools

| Tool | Who | Does |
|---|---|---|
| `monitor_create` | member | A monitor: `name`, `type`, and the fields above. |
| `monitor_list` | viewer | Every monitor with status, last check, uptime, latency, bars and sparkline; `down`, the org's recent `incidents`, and `settings`. |
| `monitor_get` | viewer | One monitor, with its last 20 incidents and checks. |
| `monitor_update` | member | Change fields (null puts one back to its default). |
| `monitor_delete` | member | The monitor and its history; an app's own one excludes the app. |
| `monitor_pause`, `monitor_resume` | member | Stop and start checking. |
| `monitor_checks` | viewer | History over `range` (`1h`, `24h`, `7d`, `30d`, `90d`): buckets, uptime, raw checks. |
| `monitor_settings` | member | `auto_monitors`, `exclude_apps`. |

Every change is in the [audit log](../operations/audit.md).

## Not yet

- A public status page per org.
- Checks from more than one place (a monitor runs where the org's apps run).
- `isb monitor` CLI commands: use the tools over REST or MCP, or the web UI.
