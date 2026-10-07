---
title: Operations
description: Running isb serve on a host you look after: setup, upgrades, backups, the audit log, the history, metrics and troubleshooting.
order: 5
---

These pages are for whoever looks after the machine `isb serve` runs on. The
daemon is a single binary with its state in one directory, so most of
operating it is knowing what that directory holds, what the host needs once,
and where to look when something goes wrong.

- [Setting up a host](host-setup.md): install the daemon as a service, open
  the host firewall for org bridges, and know what lives in the state
  directory.
- [Upgrading isb](upgrades.md): what to do after installing a new binary, on
  a host and on a Mac.
- [Backing up isb](backups.md): what to copy so the platform itself can be
  restored, and what to keep out of backups.
- [The audit log](audit.md): who did what, through which door, with a hash
  chain that shows tampering.
- [The history](history.md): every controller event and every incus change,
  made through isb or not, in one timeline.
- [Metrics history](metrics.md): a month of CPU, memory, network and disk
  numbers per instance.
- [Troubleshooting](troubleshooting.md): symptoms, their causes and the fix.
