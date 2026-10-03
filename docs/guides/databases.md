---
title: Databases, backups and restores
description: Run Postgres, MySQL, MariaDB, MongoDB or Redis as an app with generated credentials, back it up to S3 on a schedule, and restore it.
order: 6
nav_title: Databases and backups
---

Most apps need a database, and a database needs credentials, a data volume,
a health check and backups. isb makes one a single command: a database is an
[app](deploy-apps.md) whose source is a database engine instead of an image
or a repository. It lives in a project environment like any app, runs as one
service of the stack `<project>-<env>`, and other apps reach it by service
name. Its backups stream straight from the database to an S3-compatible
bucket.

```console
$ isb project create shop
$ isb db create pg --project shop --engine postgres          # or postgres:16
created database pg
engine:    postgres 17
host:      pg.shop-production  (pg.shop-production.acme.isb)
port:      5432
user:      pg
database:  pg
password:  secret db.pg.password (isb secret get db.pg.password / --show-password)
url:       postgres://pg:${{secret.db.pg.password}}@pg.shop-production:5432/pg
for apps:  DATABASE_URL=${{secret.db.pg.url}}
volume:    shop-production_pg_data
$ isb app create web --project shop --image docker:myapp:1 -e 'DATABASE_URL=${{secret.db.pg.url}}' --deploy
```

```text
isb db create NAME --project P [--environment E] --engine ENGINE[:VERSION] [--database D] [--user U]
              [--publish [IP:]PORT] [--no-deploy]     postgres, mysql, mariadb, mongodb, redis
isb db ls [--project P] [--json] | show NAME [--show-password] [--json] | rm NAME
```

All take `--org ORG`. In the web UI: **New database** from a project's menu,
and the database's **Database** and **Backups** tabs
([The web UI](../getting-started/web-ui.md)).

## Engines

| Engine | Image (default tag) | Port | Data | Health check |
|---|---|---|---|---|
| `postgres` | `docker:postgres:17` | 5432 | `/var/lib/postgresql/data` (`PGDATA` a subdirectory) | `pg_isready` |
| `mysql` | `docker:mysql:8.4` | 3306 | `/var/lib/mysql` | `mysqladmin ping` |
| `mariadb` | `docker:mariadb:11.4` | 3306 | `/var/lib/mysql` | `mariadb-admin ping` |
| `mongodb` | `docker:mongo:8.0` | 27017 | `/data/db` | `mongosh` ping |
| `redis` | `docker:redis:7.4` | 6379 | `/data` (RDB snapshots, `--save 60 1`) | `redis-cli ping` |

`--engine ENGINE:TAG` (`version` in the tools) picks another tag of the
official image. The deploy pins the tag to its digest, as for image apps.

What a database gets that an image app does not:

- **Its data on a named volume**, `<stack>_<db>_data`, mounted at the
  engine's data directory. Deleting the database keeps the volume.
- **One replica, rolled out stop-first.** Two live copies of a database on
  one data directory corrupt it, so `replicas` is at most 1.
- **A health check** per engine, with a two-minute start period for the
  first initialization. A deploy is done when the check passes.
- **Credentials, generated on create**, kept as org secrets and delivered as
  `{secret: NAME}` environment (on OCI images that is incus instance config:
  the documented trade-off in [Secrets](secrets.md)):

  | Secret | Holds |
  |---|---|
  | `db.<name>.password` | The user's password (Redis: `requirepass`; MongoDB: the root user's). |
  | `db.<name>.root-password` | MySQL and MariaDB: `root`'s password. |
  | `db.<name>.url` | The internal connection URL, password included, for apps: `DATABASE_URL=${{secret.db.<name>.url}}`. |

  The database's own engine reads them only on first start, when the data
  directory is empty. So a database deleted and created again with the same
  name **reuses** its passwords (they are kept with the volume), and
  changing a password means changing it in the database too.
- **No published port by default.** `--publish [IP:]PORT` (`publish` in
  `database_create`) publishes it on the host (default address
  `127.0.0.1`), load-balanced like any app port; `isb db show` then lists
  the external URL.

`isb db show NAME` (`database_get`) prints the connection details with the
password as a reference; `--show-password` (`reveal: true`) reads the secret
and prints the value. Members of the org may do that, as they may read any
org secret ([Users, roles and superadmins](../concepts/access.md#roles)); the
read is in the audit log.

Everything else is the app tools: `isb app deploy pg`, `isb app update pg`
(env such as `POSTGRES_INITDB_ARGS`, resources, the version), `isb app
rollback`, `isb db rm` (`app_delete`). The engine, the database name and the
user are fixed: they live in the data volume. A new major version over old
data is the engine's business (Postgres refuses to start; restore a backup
into a new database instead).

Known issue: MongoDB 8.0 and 8.2 refuse to start on Linux 6.19 and newer
(SERVER-121912).

## Backups

Backups go to an S3-compatible **destination**: AWS S3, Cloudflare R2,
Backblaze B2, MinIO, RustFS, Garage. The daemon runs the engine's own dump
inside the database's instance, compresses the stream and uploads it; the
database's org needs no network path to the bucket, and the dump is never
written to the host's disk.

```console
$ export ISB_S3_SECRET_KEY=...            # or type it on stdin
$ isb backup dest create offsite --endpoint https://s3.eu-central-1.amazonaws.com \
    --region eu-central-1 --bucket acme-backups --prefix isb --access-key AKIA...
created destination offsite
test: ok (212 ms)
$ isb backup create pg-nightly --database pg --destination offsite --schedule '0 3 * * *' --keep 14
created backup pg-nightly; next run 2026-10-04T03:00:00Z
$ isb backup run pg-nightly                # now, following the log
$ isb backup files pg-nightly
TAKEN                 SIZE      KEY
2026-10-03T09:09:12Z  47820226  isb/acme/pg-nightly/pg-20261003T090912Z.postgres.gz
```

```text
isb backup dest create NAME --endpoint URL --bucket B [--region R] [--prefix P] [--path-style]
              (--access-key ID | --access-key-secret S --secret-key-secret S) [--create-bucket] [--no-test]
                                           the secret key comes from $ISB_S3_SECRET_KEY or stdin
isb backup dest ls [--json] | rm NAME | test NAME
isb backup create NAME (--database DB | --volume V) --destination D --schedule CRON [--keep N]
              [--compression gzip|zstd|none] [--timezone +HH:MM]
isb backup update NAME [--schedule CRON] [--keep N] [--destination D] [--enable|--disable]
isb backup ls [--json] | files NAME [--json] | rm NAME
isb backup run NAME [-d]                   back up now; follows the run's log
isb backup runs [NAME] [--restores] [--json] | logs [NAME] [RUN] [--restore] [-f]
```

**Destinations** (`backup_destination_*`): endpoint, region (default
`us-east-1`), bucket, key prefix, and `path_style` (`endpoint/bucket/key`:
MinIO and most self-hosted stores; off means `bucket.endpoint/key`, as AWS
and R2 want). The key pair is stored as the org secrets
`backup.<name>.access-key` and `backup.<name>.secret-key` (or name existing
secrets with `--access-key-secret`/`--secret-key-secret`). Creating one
writes, reads back and deletes a small object unless `--no-test`;
`--create-bucket` creates the bucket first. An endpoint on the daemon's own
host (loopback, link-local) is refused unless the local CLI or a platform
admin creates it, so an org cannot point the daemon at host-local services.

A backup can also take a named **volume** instead of a database
(`--volume`): its snapshot is exported and streamed the same way, and it
restores staged beside the volume ([Volumes](volumes.md#backups)).

**Backups** (`backup_create`): a database, a destination, a cron schedule
([Scheduled jobs](jobs.md#schedules)), `keep` (default 7), `compression`
(`gzip`, the default; `zstd`; `none`), `timezone`, `enabled`. Each run:

1. dumps inside the database's running instance, as root, with the
   instance's own credentials (never argv from the daemon), for at most
   12 hours:

   | Engine | Dump | Restore |
   |---|---|---|
   | postgres | `pg_dump --format=custom -Z0` | `pg_restore --clean --if-exists --no-owner --no-privileges` |
   | mysql | `mysqldump --single-transaction --routines --triggers --events` | `mysql` |
   | mariadb | `mariadb-dump --single-transaction --routines --triggers --events` | `mariadb` |
   | mongodb | `mongodump --archive --db <database>` | `mongorestore --archive --drop`, renamed to the target's database |
   | redis | `SAVE`, then the RDB file | the RDB file swapped in, the server restarted |

2. compresses it and streams it to
   `<prefix>/<org>/<backup>/<db>-<YYYYMMDDTHHMMSSZ>.<engine>.gz|.zst`:
   one 16 MiB part in memory at a time, a single `PUT` when it fits in one,
   a multipart upload otherwise (aborted on any failure). Requests are
   signed with AWS Signature Version 4.
3. checks the object's size with `HEAD`,
4. deletes the oldest of the backup's objects beyond `keep` (only objects
   named as above; anything else under the prefix is left alone),
5. records the run (status, trigger, duration, key, sizes, log) and emits
   `backup.succeeded` or `backup.failed` on the event feed
   ([Notifications](notifications.md) can tell you).

A backup that comes due while the previous one is still running is skipped
(recorded as `skipped`). `isb backup runs NAME` and `isb backup logs NAME
[RUN]` show the history. Deleting a backup (`backup_delete`) keeps its
objects in the bucket.

## Restores

```console
$ isb backup restore pg-nightly --new pg-copy                   # a new database beside pg
$ isb backup restore pg-nightly --key isb/acme/pg-nightly/pg-20261003T090912Z.postgres.gz --into pg
restore the backup into pg, replacing its data? [y/N]
```

```text
isb backup restore [BACKUP] [--destination D --key K] (--into DB [-y] | --new NAME [--project P]
              [--environment E]) [-d]       --into asks before replacing the database's data
```

`backup_restore` takes a backup (its newest object, or `key`), or a
`destination` and `key` (for a backup deleted since), and restores into:

- `target`: an existing database of the same engine (MySQL and MariaDB dumps
  restore into each other). Its data is replaced, so the tool needs
  `confirm: true`, and the CLI asks (or `--yes`).
- `new: {name, project?, environment?, version?}`: a database created for it,
  by default in the backed-up database's project and environment with its
  version, with fresh credentials of its own. It is deployed and healthy
  before the restore starts.

The object streams from the bucket through the daemon into the restore
command's stdin. Restores are runs too (`isb backup runs --restores`, `isb
backup logs --restore RUN`) and emit `restore.succeeded` or
`restore.failed`. A volume backup is refused here: it restores staged
([Volumes](volumes.md#staged-restores)).

## Tools

| Tool | Does |
|---|---|
| `database_create` | A database app (`engine`, `version`, `database`, `user`, `publish`, `env`, `resources`), deployed unless `deploy: false`. |
| `database_list`, `database_get` | Databases with connection details; `reveal` adds the password. |
| `backup_destination_create`, `_list`, `_delete`, `_test` | Destinations. Loopback endpoints: local callers and platform admins only. |
| `backup_create`, `backup_update`, `backup_delete` | Backup schedules (`database`, or `volume`: [Volumes](volumes.md)). |
| `backup_list` | Schedules with last and next run; with `name`, the backup's objects. |
| `backup_run`, `backup_runs`, `backup_run_log` | Back up now (`wait`), the history, a run's log (`restore: true` for restores). |
| `backup_restore` | Restore into `target` (`confirm`) or `new`. |

All act in the org they name, for any member of it (volume backups: admins
and owners, see [Volumes](volumes.md#who-may)).

## On disk

```text
<org root>/backups/destinations/<name>.json
<org root>/backups/schedules/<name>/backup.json, runs/<id>.json, <id>.log
<org root>/backups/restores/runs/<id>.json, <id>.log
```

`<org root>` is the state directory for the default org and
`<state>/orgs/<org>/` for the others. Files are 0600.
