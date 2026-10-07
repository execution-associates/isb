---
title: "Volumes: snapshots, backups and staged restores"
description: Snapshot an org's named volumes now or on a schedule, back them up to S3, and restore beside the live volume, never over it.
order: 7
nav_title: Volumes
---

Data that matters lives on named volumes: an app's data, a database's
files, a workspace's home. isb protects them three ways: cheap snapshots on
the volume's own pool, now or on a schedule; backups to the org's S3
destinations; and restores that land **beside** the live volume, never over
it, so you compare and copy back only what you need.

An org's **named volumes** are the custom incus volumes in its project: an
app's volumes (`<project>-<env>_<app>_<NAME>`, [Deploy apps](deploy-apps.md#an-apps-settings)),
a database's data ([Databases](databases.md)), a workspace's home
([Workspaces](../concepts/workspaces.md#the-home-a-volume-or-a-host-folder)).

```console
$ isb --org acme volume snapshot schedule web_data --schedule '0 * * * *' --keep 24
web_data: next snapshot 2026-10-03T21:00:00Z
$ isb --org acme volume snapshot create web_data --as before-upgrade
snapshot of web_data: run 3 started
isb: running /etc/isb/pre-snapshot in web-1 (timeout 300s)
isb: snapshotting web_data as before-upgrade
run 3: succeeded
$ isb --org acme backup create web-data-nightly --volume web_data --destination offsite --schedule '0 3 * * *' --keep 14
$ isb --org acme volume restore web_data --snapshot before-upgrade
restoring web_data into web_data-restore-20261003T201304Z (restore run 4); it will be at /restore/20261003T201304Z
isb: copying snapshot web_data/before-upgrade to web_data-restore-20261003T201304Z
isb: mounted at /restore/20261003T201304Z in web-1, read-write; the live volume is untouched
run 4: succeeded
$ isb --org acme volume discard web_data 20261003T201304Z
```

In the web UI, the **Volumes** page (linked from Backups) lists the org's
volumes, and a volume's page (the Volume panel, also on a workspace's Home
tab) shows its snapshots, schedule and hook, backups and staged restores
([The web UI](../getting-started/web-ui.md)).

## Snapshots

incus snapshots of the volume, on its own pool (cheap, and lost with the
pool: back up for that). On a `dir` pool every snapshot is a full copy of
the volume, as large and as slow as the volume itself; see
[Workspaces](../concepts/workspaces.md#the-home-a-volume-or-a-host-folder).

- **Scheduled** (`volume_snapshot_schedule`): a cron schedule
  ([Scheduled jobs](jobs.md#schedules)), `timezone`, `keep` (default 7; 1 to
  1000), `missed_grace`. They are named `auto-<YYYYMMDDTHHMMSSZ>`, and after
  each one the oldest `auto-` snapshots beyond `keep` are deleted.
- **Now** (`volume_snapshot_create`): named as given (`before-upgrade`) or
  `manual-<stamp>`. Never pruned: kept until deleted
  (`volume_snapshot_delete`).
- Every snapshot is a **run** with a log (`volume_snapshot_runs`,
  `volume_snapshot_run_log`): the hook's output, the snapshot's name, what
  was pruned. A run due while the previous is still going is `skipped`.

## The pre-snapshot hook

Before every snapshot, and before every backup's snapshot, each **running**
instance using the volume runs its `/etc/isb/pre-snapshot` if it has one: as
root, with `ISB_VOLUME`, `ISB_REASON` (`snapshot` or `backup`) and
`ISB_SNAPSHOT` in its environment. It is how an image makes its own state
consistent first (a SQLite checkpoint, a flushed session file); isb knows
nothing about what the instance runs. Stopped instances need no hook.

- Its output goes to the run's log.
- `hook_timeout` (default `5m`, at most `1h`): past it the hook is killed
  (SIGKILL to the hook's process; anything it started itself is not).
- A hook that fails (exits non-zero, is not executable, times out) is
  reported in the log, and by default the snapshot is taken anyway.
  `hook_required: true` makes a failure stop the snapshot (and the backup):
  the run fails and says why.

## Backups

A volume backup is a [backup](databases.md#backups) whose source is a
`volume` instead of a `database`: same destinations, schedules, `keep`,
compression, runs, logs and `backup.succeeded`/`backup.failed` events, listed
on the Backups page beside the database backups. Each run:

1. runs the hook and takes a snapshot `isb-backup-<stamp>`,
2. copies it to a temporary volume `<volume>-backup-<stamp>` (nothing writes
   to it: the export is exactly the snapshot),
3. has incus export that volume (an uncompressed incus backup tarball,
   written by incus to its own backups directory on the host while it is
   read; incus expires a forgotten one after a day), compresses the stream
   and uploads it to
   `<prefix>/<org>/<backup>/<volume>-<YYYYMMDDTHHMMSSZ>.volume.tar[.gz|.zst]`,
4. deletes the export, the temporary volume and the snapshot (also on
   failure), checks the object with `HEAD`, and deletes the oldest of the
   backup's objects beyond `keep` (only objects named as above).

`backup_restore` refuses a volume backup: volume backups restore staged,
below.

## Staged restores

`volume_restore` (`isb volume restore`) restores a `snapshot`, a volume
`backup` (its newest file, or `key`), or a `destination` and `key` (a backup
deleted since), into a **new volume** `<volume>-restore-<stamp>`:

- mounted read-write at `/restore/<stamp>` in the instance using the volume
  (a running one first, or `instance`), as a disk device
  `isb-restore-<stamp>`;
- left **detached** when that instance is stopped (or nothing uses the
  volume);
- never written over the live volume. Compare, copy back what you need
  (`diff -r /restore/<stamp> ~`), then discard it.

A snapshot restores as a copy of the snapshot; a backup streams from the
bucket through the daemon into an incus import. File ownership is kept.
Restores are the org's restore runs (`isb backup runs --restores`, the
Backups page's Restores), and emit `volume.restore.staged` or
`volume.restore.failed`.

The staged volume says what it is in its config: `user.isb.restore-of`,
`-from` (`snapshot:<name>` or `backup:<key>`), `-stamp`, `-by` and
`-instance`. `volume_restore_list` lists them; `volume_restore_discard`
(`isb volume discard VOLUME STAMP`) detaches it, removes the empty mount
point and deletes it, and refuses any volume without those keys.

## Who may

Members and viewers read: volumes, snapshots, runs, logs, staged restores.
Taking and deleting snapshots, the schedule and hook, volume backups
(create, update, run, delete) and restores are for the org's **admins and
owners** (and platform admins, superadmins, the local socket). Every call is
in the audit log with the volume, snapshot, backup and stamp it named.

isb takes the snapshots and exports, so an org's incus project allows them
(`restricted.snapshots` and `restricted.backups`); a project without them
gets both on its first snapshot.

## History

Events land in the [history](../operations/history.md) as
`volume.snapshot.created`, `volume.snapshot.failed`,
`volume.snapshot.deleted`, `volume.restore.staged`, `volume.restore.failed`
and `volume.restore.discarded`, about the volume (object type `volume`);
backups are `backup.succeeded`/`backup.failed` as for databases.

## Tools

| Tool | Does |
|---|---|
| `volume_list` | The org's volumes: instances using each, its snapshot schedule, staged restores. |
| `volume_get` | One volume: instances, settings and next run, snapshots, staged restores, backups of it. |
| `volume_snapshot_list` | Snapshots, newest first, with kind (`auto`, `manual`, `other`). |
| `volume_snapshot_create` | Snapshot now (`snapshot` name; `wait`). |
| `volume_snapshot_delete` | Delete a snapshot. |
| `volume_snapshot_schedule` | Schedule, `keep`, `hook_timeout`, `hook_required` (a merge patch; `schedule: null` removes it). |
| `volume_snapshot_runs`, `volume_snapshot_run_log` | Snapshot runs and their logs. |
| `volume_restore` | Restore staged from a `snapshot`, `backup` (+`key`) or `destination` + `key`; `instance`; `wait`. |
| `volume_restore_list`, `volume_restore_discard` | Staged restores; detach and delete one by `stamp`. |
| `backup_create` with `volume` | A volume backup (then `backup_*` as for databases). |

## CLI

```text
isb volume show NAME                        snapshots, schedule, backups, staged restores (JSON)
isb volume snapshot create NAME [--as SNAP] [-d]
isb volume snapshot ls NAME [--json] | rm NAME SNAP | runs NAME | logs NAME [RUN]
isb volume snapshot schedule NAME [--schedule CRON | --off] [--timezone TZ] [--keep N]
                                  [--hook-timeout 5m] [--hook-required | --hook-optional]
isb volume restore NAME (--snapshot S | --backup B [--key K] | --destination D --key K) [--instance I] [-d]
isb volume restores [NAME] | discard NAME STAMP
isb backup create NAME --volume V --destination D --schedule CRON [--keep N] [--compression C]
```

All take the global `--org` and go through `isb serve`. `isb volume
create|ls|inspect|rm` talk to incus directly ([CLI](../reference/cli.md)).

## On disk

```text
<org root>/volumes/<volume>/volume.json      settings and the scheduler's anchor
<org root>/volumes/<volume>/runs/<id>.json, <id>.log
```

Volume backups live with the other backups (`<org root>/backups/schedules/`),
staged restores' runs with the other restores (`backups/restores/runs/`).
Files are 0600.
