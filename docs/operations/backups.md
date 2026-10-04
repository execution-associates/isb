---
title: Backing up isb
description: What to copy so the platform itself can be restored, what isb backs up for you, and what must stay out of a backup.
order: 3
---

isb backs up your workloads' data for you: database dumps and volume exports
to S3, on a schedule ([Databases](../guides/databases.md#backups),
[Volumes](../guides/volumes.md)). It does not back up itself. The platform's
own state (users, orgs, apps, stack definitions, secrets, the audit log) is a
handful of files on the host, and keeping a copy of them is the operator's
job. This page lists them.

## What to copy

| What | Where | Why |
|---|---|---|
| The state directory | `--state-dir`, default `~/.local/state/isb` ([the map](host-setup.md#the-state-directory)) | Everything isb knows: identity, orgs, apps and deployments, stack definitions, encrypted secrets, schedules, the audit log and history, metrics, ingress certificates. |
| The service settings | `~/.config/isb/serve.env` | Listen addresses, Access, providers, ingress flags. It can hold provider client secrets; treat it like a secret. |
| Break-glass recipients | `~/.config/isb/secrets.toml` (`$ISB_SECRETS_CONFIG`) | Public keys only; without the file the daemon encrypts new values to its own key alone. |
| The secrets key, safely | see [below](#the-secrets-key) | Every stored secret is encrypted to it. |

The databases in the state directory (`isb.db`, `audit.db`,
`orgs/<org>/metrics.db`) are SQLite in WAL mode. For a consistent copy, stop
the daemon while you copy (`systemctl --user stop isb`: apps keep running,
only published ports and domains pause), or copy from a filesystem snapshot.
The state directory holds sockets (`ingress/run/admin.sock` while ingress
runs), which a copy has no use for; leave them out, or `tar` warns about each:

```console
$ systemctl --user stop isb
$ tar --exclude='*.sock' -czf isb-state.tgz -C ~/.local/state isb
$ systemctl --user start isb
```

What is **not** in the state directory, and how it is covered:

- **Instances and named volumes** live in incus storage. Back up databases and
  volumes with isb's scheduled backups; anything else with incus' own tools.
- **The local registry's images** are on the volume `registry-data` in the
  incus project `isb-system`. Apps built from git can be rebuilt from their
  repositories; the registry's CA and keys are in `<state>/registry/`.
- **Workspace homes on host folders** (`--workspace-home-root`) are the
  host's to back up; isb takes no snapshots of them
  ([Workspaces](../concepts/workspaces.md#the-home-a-volume-or-a-host-folder)).
  Those backups hold the org's data: keep them with the same care as the
  daemon's state.

## The secrets key

The daemon's age key decrypts every stored secret. A backup that holds both
the state directory and the key decrypts everything in it, so:

- **Keep the plaintext key out of unencrypted backups.** It is
  `~/.config/isb/age.txt` unless `ISB_AGE_KEY`, `ISB_AGE_KEY_FILE` or a systemd
  credential provides it ([the lookup order](../guides/secrets.md#the-daemons-key)).
- **The encrypted credential** that `isb serve install` makes
  (`~/.config/isb/isb-age-key.cred`, systemd 256 or later) is bound to this
  machine and user: it cannot be decrypted anywhere else, so it does not help
  a restore onto new hardware.
- **Add a break-glass recipient** before you rely on either. Every value is
  then also encrypted to a key you keep offline, so the secrets can be
  recovered with `age -d -i KEY <state>/orgs/<org>/secrets/<name>.age`
  without the daemon. After adding one, restart the daemon and run
  `isb secret reencrypt --all` so existing values are encrypted to it too
  ([Break-glass recipients](../guides/secrets.md#break-glass-recipients)). The
  daemon warns at start-up while there is none.

Workspace tokens (`<org root>/workspaces/<name>.token.age`) are encrypted to
the same key.

## A control plane and its servers

A control plane's `<state>/servers/pki/` holds the CA every agent trusts. Lose
it and the control plane can no longer reach its servers: restore it from
backup, or re-run `isb server add` on each box (which reissues the agent's
certificate under a new CA) and recreate the placement
([Servers](../guides/servers.md#failure-modes)).

The control plane stores no secret value, stack definition or workload state
of an org placed on a server. Each server keeps those itself, under
`/var/lib/isb/state`, with its own age key at
`/var/lib/isb/.config/isb/age.txt`. Back up each server the same way, and give
each its own break-glass recipient (`isb secret reencrypt` there).

## The audit log's head

Anyone who can write `audit.db` can rebuild a whole hash chain. `isb audit
verify` prints each chain's report with its head (`[id, hash]`); copy it somewhere else, a
ticket or another host, to pin the log up to that point
([The audit log](audit.md#storage-retention-tamper-evidence)).

## Restoring

Put the state directory and `~/.config/isb/` back for the daemon's user, make
the secrets key available (the plaintext file, `ISB_AGE_KEY`, or a new systemd
credential from it with `isb serve install`), and start the daemon. It resumes
every stack from its definitions; instances that are already running are left
alone, and missing ones are recreated. Then restore databases and volumes
from their own backups.
