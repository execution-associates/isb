---
title: Uptime monitoring
description: Hear about it when an app or stack service users reach is down, with uptime history, incidents, certificate expiry warnings, and a heartbeat for when the host itself dies.
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
within a minute, and so does every service of a compose stack
(`stack-<stack>-<service>`), so with a channel that hears `monitor.*` there
is nothing else to set up:

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
| `service` | a compose stack's service, by `stack` and `service`: the same, for its domains | the same |
| `http` | any `url` (`http://` or `https://`) | the same |
| `tcp` | `host` and `port` | a connection opens |

An `app` monitor follows the app: rename its domain, add one, scale it, and
the monitor checks what is served now. With no domain it checks the app's own
endpoint instead: its published port, else a replica in rotation (and with no
replica in rotation it is down, saying so). A `service` monitor follows a
service of a stack deployed with `stack_deploy` the same way; its own
endpoint is where the ingress sends the domain's requests (a replica in
rotation, on the domain's port).

### Which path an `app` or `service` monitor requests

1. The monitor's `path`, when it sets one.
2. Otherwise the service's own health path: when its compose `healthcheck`
   requests an HTTP URL on `127.0.0.1`, `localhost` or `[::1]` at the port
   the domain routes to (no port is 80), that URL's path and query. isb
   finds it in `[CMD, ...]`, `[CMD-SHELL, "..."]` and plain-string tests:
   `curl -f`, `wget -q --spider`, `wget -qO-`, a Python `urlopen(...)`, or a
   hand-written `GET <path> HTTP/1.x` to bash's `/dev/tcp/127.0.0.1/<port>`.
   A URL on another port or host, a variable in it, or no URL at all
   derives nothing.
3. Otherwise the domain's path (`/` without a domain).

A derived health path `P` meets the domain's path `D` like this:

| The domain | Public URL | Own endpoint |
|---|---|---|
| `D` is `/` | `P` | `P` |
| `strip_prefix: true` | `D` + `P` | `P` |
| `P` is under `D` (`/api/health` under `/api`) | `P` | `P` |
| anything else (`/healthz` under `/sso`) | not checked | `P`, with the note "the health path P is not under the domain's path D: checked the service's own endpoint" |

A derived path says so in the check's `note` ("/healthz is the path of the
service's healthcheck"), and the check's `url` shows the path requested. On
a route that strips its prefix, the own-endpoint check requests the path
without it, as the ingress hands it to the replica (`path: /api/status` on
a `/api` route that strips asks the replica for `/status`). Set `path` to
check something else; `path` always wins.

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
| `paused` | `false` | a paused monitor is not checked; its history stays. A monitor whose app or service is stopped rests on its own (status `stopped`) |

```sh
# Over REST (or MCP): any URL, with a keyword.
curl -H "Authorization: Bearer $ISB_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "docs", "type": "http", "url": "https://docs.example.com/", "keyword": "Welcome"}' \
  https://isb.example.com/orgs/acme/api/v1/tools/monitor_create
```

## Pending, down, up, and nothing in between

- A new monitor starts **pending** and stays so until its first successful
  check. Checks that fail before that are *pending* checks: shown grey in the
  UI ("Waiting for the first successful check"), never an incident, never a
  `monitor.down`, not counted toward `failure_threshold`, and left out of
  uptime percentages and the uptime bars' red. The first success makes it
  **up**, without a `monitor.up`. After that the thresholds below apply as
  usual, so real downtime is never hidden.
- An `app` or `service` monitor (including an app's or service's own) does
  not even look at its target while it is pending and the target has no live
  deployment (no healthy replica in rotation); it records a pending "waiting
  for the app's first live deployment" (or "the service's first replica in
  rotation") check instead. Once it has been up, a failed rollout that leaves
  the old revision serving does not page, and a real outage does.
- An `app` or `service` monitor whose target is scaled to 0 (Stop, or
  `stack_scale` to 0 replicas) is **stopped**: not checked and never paging.
  Stopping closes an open incident without a notification; starting again
  begins as a new monitor does, pending until its first success. Only a scale
  to 0 counts: a target that crashed, or lost its last replica, still pages.
- A monitor still pending with only failures after 30 minutes is flagged **never came
  up** (`never_up: true` in `monitor_list` and `monitor_get`; shown red as
  "Never came up"). It opens an incident dated from its first check and sends
  one `monitor.down` saying so; it stays pending, and its first success sends
  `monitor.up` and closes the incident.
- `failure_threshold` failures in a row make a monitor **down**: channels get
  `monitor.down` once, and an incident opens, dated from the first failure.
- `recovery_threshold` successes in a row make it **up**: channels get
  `monitor.up` with how long it was down, and the incident closes. One
  success between failures does not count (hysteresis).
- Notifications alternate: never two downs or two ups in a row, and an up only
  after a down was sent. A new monitor's first success is not news.
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
match them), a `service` monitor's about that stack and service; other
monitors' are about the stack `@monitors`, service = the monitor. The webhook body carries the details:

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

## Apps' and stack services' own monitors

Every app with a served domain gets `app-<name>`, and every service of a
compose stack with a served domain gets `stack-<stack>-<service>` (a `service`
monitor): GET its first domain, at the service's health path when its
healthcheck names one ([which path](#which-path-an-app-or-service-monitor-requests)),
`200-399`, every 60 s, down after 2 failures.
Each is made within a minute of the domain being served. Edit it like any
other monitor (it stays the app's or service's). It goes away when its target
does, or no longer declares a domain; a domain that is briefly not served (a
conflict, no replica) keeps it.

- A stack service's monitor name is `stack-<stack>-<service>`, the service
  spelled as its instances are. When that is over 63 characters, or another
  stack service's monitor already has it (`a-b`/`c` and `a`/`b-c`), it is cut
  to fit and ends in a 6-digit hash of `<stack>/<service>`. It never looks
  like an app's, and a monitor you made with the name keeps it.
- Stacks that apps render (a project environment's `<project>-<env>`, a
  preview's) are covered by the apps' own monitors, and the org's tunnel
  stack (`isb-tunnel`) serves nothing of its own: neither gets `service`
  monitors. Services without a domain (a database, a cache) get none.
- A workspace's published ports get no monitor of their own: they are
  development servers that stop with the workspace or the session that
  started them, so a monitor would page every time one stops. Give one an
  `http` monitor on its host when it should be watched.

Deleting an own monitor adds its target to the org's exclusions so it does
not come back (`exclude_apps`, or `exclude_services` as `<stack>/<service>`);
`monitor_settings` turns the whole thing off (`auto_monitors: false`) or edits
the exclusions.

Deleting the target itself (`app_delete`, `stack_remove`, `project_delete`,
`environment_delete`) deletes the monitors that follow it, its own and any
`app` or `service` monitor made for it, with their history, unless
`keep_monitors` is set. Its exclusion goes too, so an app made again under the
name gets its own monitor again. `http` and `tcp` monitors are left alone.

### Behind Cloudflare Access

A domain behind [Cloudflare Access](remote-access.md) answers for Access, not
the app: a sign-in redirect (to `*.cloudflareaccess.com`), or a refusal, a 403
page carrying Access' `cf-access-*` headers or, for an app Access fronts with
OAuth, a 401 whose `WWW-Authenticate` names its `cloudflare-access-protected-resource`
metadata. isb recognises all three and never counts them as up:

- **Give monitors a service token** (recommended): create an Access service
  token allowed by the application's policy, and store it as the org secrets
  `CF_ACCESS_CLIENT_ID` and `CF_ACCESS_CLIENT_SECRET`. Every `app` and
  `service` monitor that sets no Access headers of its own presents them, so
  the check goes through Access to the app, as users do: one request, end to
  end. Any monitor can also name them in `headers` (`CF-Access-Client-Id`,
  `CF-Access-Client-Secret`). When Access still stops a request that
  carries the token, its policy does not allow that token: users still get
  through, so the monitor checks hop by hop (below) and its note says to
  allow the token in the Access application's policy.
- **Without a token**, an `app` or `service` monitor checks hop by hop, and
  is up only when every hop is:

  | Hop | Passes when | Down as |
  |---|---|---|
  | `edge` | the domain answered with Access' redirect or refusal: its DNS and Cloudflare's edge work | (the public check failing any other way is the check's own error) |
  | `tunnel` (an org on the `cloudflare-tunnel` ingress) | the org's cloudflared (stack `isb-tunnel`) has a healthy replica, and its readiness endpoint reports connections to Cloudflare | "tunnel: the org's Cloudflare tunnel is not running", "tunnel: not connected (...)" |
  | `ingress` | the ingress listener the domain comes in on (the org's tunnel listener, the address cloudflared forwards to) answers the domain's public path, with the domain as the Host header, as the monitor expects | "ingress: HTTP 502 (expected 200-399)" |

  The check's `hops` list each hop (`hop`, `ok`, `detail`), its `via` names
  the listener (`ingress http://10.64.3.1:8480`), and its `note` says
  "checked hop by hop (Cloudflare edge, tunnel, ingress); the Access policy
  is not verified: add CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET
  secrets for an end-to-end check". The ingress is asked for the public path
  (it strips a route's prefix itself) and, for an HTTPS domain on a tunnel,
  with `X-Forwarded-Proto: https`, as cloudflared sends it.

  What the hops cannot see: the Access policy itself, and Cloudflare's own
  routing from the edge into the tunnel (the tunnel's public hostname rules,
  which Access answers in front of). The tunnel hop reads cloudflared's
  readiness at the replica's address (its image serves metrics on the first
  free port of 20241-20245 on every address); when that cannot be read, a
  healthy cloudflared passes and the hop says the connection is not
  verified. With no listener address for the domain, the service's own
  endpoint stands in for the ingress (hop `replica`), and the note says so.

  An `http` monitor is down with "redirected to Cloudflare Access sign-in"
  or "refused by Cloudflare Access", since that is all it can see.

## Where checks run, and what they may reach

Checks run in the daemon that runs the org's apps, `isb serve` on the host,
which sees the app's real URL from the network the app lives on. What the
daemon cannot report is its own death: a
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
- An app's or stack service's own endpoint (a replica, a published port) is
  found by reference, not typed, so it is reached directly whatever the
  policy, and so is the ingress listener a domain comes in on. An `app` or
  `service` monitor whose domain resolves to a private address while
  private targets are refused checks hop by hop as
  [behind Access](#behind-cloudflare-access) does, without the edge hop
  (the tunnel, then the ingress), and says so.

## History

Per org, in `<state>/orgs/<org>/monitors/monitors.db` (SQLite): every check
for 7 days, hourly rollups (checks, successes, latency p50 and p95) for 90
days (pending checks are kept but never counted), incidents for 90 days after
they end. A monitor checked every 30 s keeps
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
stops working. With [several hosts](agents.md#several-hosts), give each its
own check.

## Tools

| Tool | Who | Does |
|---|---|---|
| `monitor_create` | member | A monitor: `name`, `type`, and the fields above. |
| `monitor_list` | viewer | Every monitor with status, last check, uptime, latency, bars and sparkline; `down`, the org's recent `incidents`, and `settings`. |
| `monitor_get` | viewer | One monitor, with its last 20 incidents and checks. |
| `monitor_update` | member | Change fields (null puts one back to its default). |
| `monitor_delete` | member | The monitor and its history; an app's or stack service's own one excludes it. |
| `monitor_pause`, `monitor_resume` | member | Stop and start checking. |
| `monitor_checks` | viewer | History over `range` (`1h`, `24h`, `7d`, `30d`, `90d`): buckets, uptime, raw checks. |
| `monitor_settings` | member | `auto_monitors`, `exclude_apps`, `exclude_services`. |

Every change is in the [audit log](../operations/audit.md).

## Not yet

- A public status page per org.
- Checks from more than one place (a monitor runs where the org's apps run).
- `isb monitor` CLI commands: use the tools over REST or MCP, or the web UI.
