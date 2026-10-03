# The history

`isb serve` keeps a persistent history of everything that happens to what it
runs, so the current state can always be traced back: how a service got
here, and who deleted that container or image.

One timeline, four sources:

| Source | What | Actor |
|---|---|---|
| `controller` | every event the stack controller emits: deploys, rollouts and each replica they create or retire, health changes, restarts, failures, backups, jobs, certificates, volume snapshots and staged restores (`volume.*`, about the volume: [volumes.md](volumes.md)), with their `kind` (`deploy.succeeded`, ...) | `isb` |
| `incus` | every incus **lifecycle event in every project**, including changes made outside isb (`incus delete`, `incus image alias delete`, the incus UI): `instance-created/started/stopped/restarted/updated/deleted`, `instance-exec`, `image-deleted`, `image-alias-deleted`, `storage-volume-*`, `network-*`, `profile-*`, `project-*`, ... | the incus **requestor**: the unix user for the local socket, or the TLS/OIDC user with its protocol |
| `audit` | the audit rows ([audit.md](audit.md)): tool calls, sign-ins, account and token changes | the isb user, token, `local(uid N)`, `webhook:...` |
| `marker` | what isb itself saw: `serve.started`, `serve.stopped`, `incus.gap` (events between two times not observed, and why), `history.dropped` (events lost to a full queue) | `isb` |

## What is not seen

- **incus keeps no history of its own.** Lifecycle events emitted while no
  `isb serve` is running are not captured. Every such stretch is explicit:
  at start the daemon records an `incus.gap` from the last row it had (and
  says whether the daemon stopped cleanly), and when the event stream
  breaks it records the gap once it reconnects (backoff 1 s to 32 s).
- For a record that does not depend on isb running, turn on incus' own
  logging to Loki (`incus config set loki.api.url=... loki.types=lifecycle`)
  as belt and braces.
- A deployment's build output (level `log` events) is not kept here; each
  deployment keeps its own log (`app_deployment_log`).
- **Routine repeats are folded.** isb's own reconciliation execs
  `systemctl is-active` (and similar) in every replica every few seconds,
  which would be most of the history. For `instance-exec`,
  `instance-file-pushed`, `instance-file-retrieved`,
  `instance-log-retrieved` and `instance-metrics-retrieved`, the first event
  per instance, program (or file path) and requestor in an hour is recorded
  as it happens, and the rest are counted and recorded as one summary row
  (`details.repeats: {count, from, to}`) when the hour ends. A different
  program (someone running `sh`) is its own row at once. Everything else is
  recorded one by one.

## Never secrets

Incus contexts can carry configuration. Before a context is stored, keys
named `environment.*`, `environment`, `env`, `cloud-init.*`,
`user.user-data`, `user.vendor-data`, `user.isb.create-token`, and anything
whose name contains `secret`, `password`, `token`, `passphrase` or
`private` are dropped; a command line keeps only the program and the number
of arguments (`{"program": "sh", "args": 2}`), never the arguments; strings
are cut at 256 characters. Controller messages name things, never values.

## Who sees what

| Caller | History rows (controller, incus, markers) | Audit rows |
|---|---|---|
| platform admin, the unix socket, the host CLI | all, host-level included | all |
| org owner, admin | their orgs' | their orgs' |
| org member, viewer | their orgs' | none |

A row belongs to an org when it happened in the org's incus project
(`isb-<org>`), or (controller events) to one of its stacks. Host-level rows
(org `null`) are platform-admin only: images and aliases, storage pools,
networks, profiles and projects, the `default` project, `isb-system`, and
any project isb does not manage.

## Storage, retention, size

- The `history` table in `<state>/audit.db`, next to the audit rows: append
  only (triggers refuse `UPDATE`, and `DELETE` outside pruning) and hash
  chained on its own chain, like the audit log. `audit_verify` and `isb
  audit verify` check both chains.
- Writes go through a bounded queue (10,000 rows) on their own thread, so
  the controller and the event stream never wait on the disk. A full queue
  drops rows, counts them, and records a `history.dropped` marker with the
  count.
- **Retention**: rows older than `--history-retention` (default `365d`)
  are pruned at start and hourly, and past `--history-max-rows` (default
  5,000,000) the oldest go first. Pruning keeps the chain anchored.
- **Disk use, measured on titan** (39 instances in several projects, other
  people's sandboxes busy, one 2-replica test stack, 2026-10-03): without
  folding, isb's own probes alone made about 48 rows a minute (about 70,000
  a day for one small stack); with folding, 1.3 rows a minute over an
  8-minute quiet stretch, plus one summary row per routine kind, instance
  and program each hour, so a few thousand rows a day. A row takes about
  1.5 KB on disk with its indexes and the WAL, so that is a few MB a day,
  and the default bound (5,000,000 rows) caps the table at roughly 7 GB.

## Queries

- `history_query` (a tool, so MCP, REST, `/orgs/<org>/...`): filters `org`,
  `platform` (host-level rows only), `object` (an instance, image, alias,
  volume, stack, app or service name: substring, or the whole name with
  `exact`), `kind` (glob on the kind or audit action: `instance-*`,
  `deploy.*`, `secret_*`), `source` (`audit`, `controller`, `incus`,
  `marker`, comma-separated), `actor` (glob), `since`/`until` (unix ms),
  `limit` (default 100, at most 1000). Newest first, or oldest first with
  `ascending`; page with `before` = the `next` you got. `correlate: true`
  adds `inferred` to incus instance events: the audit row in the same org
  within two minutes before whose target names the instance's stack or app
  (`{audit_id, action, actor, seconds_before, why}`), labelled as
  inferred, by time and name.
- `GET /api/v1/history/stream[?org=ORG]`: new items as server-sent events
  (`event: history`), only what the caller may read; the event id resumes
  the stream.
- The CLI reads `<state>/audit.db` directly (as the daemon's user):

```text
isb history NAME                     NAME's whole timeline, oldest first, with inferred causes
isb history [--object NAME [--exact]] [--org ORG | --platform] [--source incus,audit]
            [--kind GLOB] [--actor GLOB] [--since 24h] [--until 1h] [-n 50] [--json]
isb history --export [filters]       every match as JSON lines, oldest first
```

For example, who created and deleted an image alias, and when (markers
are part of every object's timeline, so the stretches nobody watched show):

```text
$ isb history --object isbtest-p53-dev-base --exact
TIME                     SOURCE  ORG  KIND                 OBJECT                ACTOR    LEVEL  MESSAGE
2026-10-03 09:38:57.310  incus   -    image-alias-deleted  isbtest-p53-dev-base  stephan
2026-10-03 09:38:56.248  incus   -    image-alias-created  isbtest-p53-dev-base  stephan
2026-10-03 09:38:37.541  marker  -    serve.started                              isb      info   isb serve 0.7.0 started
```

The incus requestor is the unix user behind the socket, so changes isb
makes show its own user too (the daemon runs as one); the controller
events and the inferred audit row say which were isb's.

## The web UI

**History** under each org (every member) and in **Platform** (everything,
host level only, or one org): a source filter (audit log, isb events,
incus changes, gaps and restarts), object, kind and actor filters, time
range, older entries on demand, a live tail, details and the likely cause
of an incus change on a click, and **Export JSONL**. `/orgs/ORG/audit`
opens it filtered to the audit log.

## Settings

| Flag | Environment | Default |
|---|---|---|
| `--history-retention` | `ISB_HISTORY_RETENTION` | `365d` |
| `--history-max-rows` | `ISB_HISTORY_MAX_ROWS` | `5000000` |
