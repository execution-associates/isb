# Scheduled jobs

A job runs a command on a cron schedule against an app (or any stack
service) of an org: a migration check, a cleanup, a report.

```console
$ isb job create prune --schedule '0 4 * * *' --app web -- ./manage.py prune --days 30
created job prune; next run 2026-10-04T04:00:00Z
$ isb job run prune                        # now, following its output
$ isb job runs prune
RUN  STATUS     TRIGGER   STARTED               SECONDS  EXIT  DETAIL
2    succeeded  manual    2026-10-03T08:58:10Z  1.2      0
1    failed     schedule  2026-10-03T04:00:00Z  0.4      3     exit code 3
$ isb job logs prune 1
```

## Where a job runs

- `exec` (default): in a running replica of the service (the lowest slot),
  like `isb exec`: the instance's environment, secrets included.
- `run`: in a fresh one-off instance made from the service's deployed image
  (the pinned digest), with its environment, secrets (resolved from the
  org's store at run time), resources and user; no published ports,
  volumes, domains or health check. An OCI image's process is replaced by
  `sleep` so the command runs beside it, which needs a `sleep` in the image.
  The instance (`job-<job>-<hex>`, label `isb.job`) is deleted afterwards.

The target is `{app: NAME}` or `{stack: NAME, service: NAME}`, and must
exist when the job is created. The command is argv, run without a shell
(`-- sh -c '...'` for one); a tool call may give a command line, split like
a shell would. `user`, `cwd` and extra `env` apply to the command.

## Runs

- **Timeout**: `timeout` (default `10m`, at most `24h`); a run past it is
  killed and fails.
- **Concurrency**: `skip` (default) records a run that comes due while one
  is still going as `skipped`; `allow` lets runs overlap. `isb job run`
  (`job_run`) is refused while one is going under `skip`.
- **Records**: the newest `keep` runs (default 20) are kept, each with its
  trigger (`schedule`, `missed`, `manual`), status (`running`, `succeeded`,
  `failed`, `skipped`), exit code, duration and output: the first 192 KiB
  and the last 64 KiB, with a marker for what was dropped between.
  `job_run_log` reads it from an offset while the run goes.
- **Events**: `job.succeeded` and `job.failed` on the event feed, under the
  target's stack and service.
- A run the daemon was in the middle of when it stopped is marked failed
  when it starts again.

## Schedules

Jobs, [backups](databases.md#backups) and [volume
snapshots](volumes.md#snapshots) share one scheduler thread in the daemon.

- Five fields, `minute hour day-of-month month day-of-week`: `*`, numbers,
  ranges `a-b`, steps `*/n`, `a-b/n`, `a/n`, lists `a,b`; month and weekday
  names (`jan`, `mon-fri`); day of week 0-7 with both 0 and 7 Sunday. When
  both day fields are restricted, a day matching either fires (Vixie cron).
  Or an alias: `@hourly`, `@daily` (`@midnight`), `@weekly`, `@monthly`,
  `@yearly` (`@annually`). A schedule that can never fire (`0 0 30 2 *`) is
  refused.
- **Time zone**: UTC, or a fixed offset (`timezone: +02:00`). Named zones
  are not supported, so there is no daylight-saving shift: every day has
  each minute exactly once.
- A job fires once per slot. If several slots passed at once it runs once,
  for the latest.
- **Missed while the daemon was down**: when the daemon starts, a schedule
  whose latest missed slot is within its grace window (`missed_grace`,
  default `1h`; `0s` never) runs once, late (trigger `missed` when more than
  a minute late); older slots are skipped. A new or re-enabled schedule, or
  a changed one, counts from that moment.

## Tools

| Tool | Does |
|---|---|
| `job_create` | `name`, `schedule`, `timezone`, `target`, `mode`, `command`, `timeout`, `concurrency`, `keep`, `enabled`, `user`, `cwd`, `env`, `missed_grace`. |
| `job_list`, `job_get` | Jobs with their next and last run. |
| `job_update` | A merge patch of the settings; the name is fixed. |
| `job_delete` | The job and its run records (refused while it runs). |
| `job_run` | Run now (`wait` returns when it finishes). |
| `job_runs`, `job_run_log` | The history; one run's output from an offset. |

All act in the org they name, for any member of it.

## On disk

```text
<org root>/jobs/<name>/job.json
<org root>/jobs/<name>/runs/<id>.json, <id>.log
```
