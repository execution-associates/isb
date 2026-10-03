---
title: Upgrading isb
description: What to do after installing a new isb binary on a host, on a Mac, and on the servers a control plane manages, and what an upgrade changes on its own.
order: 2
---

isb is one binary, so upgrading is installing the new release the way you
installed the old one (`mise`, `cargo install isb`, or the
[releases page](https://github.com/execution-associates/isb/releases)) and
then pointing whatever runs it at the new file. Workloads keep running
throughout: stopping or restarting the daemon never stops an app.

## On a Linux host

```sh
mise use -g github:execution-associates/isb@latest   # or however you install isb
isb serve install                                     # point the unit at it, restart, wait for /healthz
isb --version
```

- **Rerun `isb serve install`.** The unit runs the binary that installed it
  by its full path, so with a version manager that keeps each version in its
  own directory (mise does), the unit keeps running the old version until you
  install again. The installer is idempotent: it rewrites the unit, keeps
  `serve.env` and the secrets key, restarts the service and waits until it
  answers. If isb lives at a fixed path that you replaced in place,
  `systemctl --user restart isb` is enough.
- **What a restart interrupts.** Apps run on in their guests. The load
  balancer and the ingress stop with the daemon, so published ports and
  domains are down for the few seconds until it is back; it then resumes
  every stack from its state directory without touching healthy instances.
  A deployment, backup, snapshot or job run the daemon was in the middle of is
  marked failed when it starts again, and notification deliveries still
  queued are lost (the event feed starts over with each run, so nothing is
  sent twice). Web terminals and SSH sessions through the daemon end.
- **Databases migrate forward on their own.** `isb.db` (identity) and
  `audit.db` (audit log and history) record a schema version and apply every
  newer migration when they are opened, each in its own transaction. A
  database written by a newer isb is refused, not downgraded: the daemon (and
  the host CLI) stops with "... is schema version N, newer than this isb
  understands; upgrade isb". So once a newer isb has started on a state
  directory, going back to an older binary needs that directory as it was
  before (a backup).
- **Stored stacks are upgraded in place.** When the daemon starts on stack
  definitions that still hold secret values inline, it moves each value into
  the org's secret store as `<stack>_<key>`, rewrites the definition with
  references, and relabels the running instances with the new revision so
  nothing rolls. It logs one line per stack; starting again changes nothing.
- **Mounts are not disturbed.** A volume device that differs from the spec
  only in `initial.copy` counts as correct, so upgrading isb or incus never
  remounts an existing volume.
- **Pinned tools follow the binary.** The ingress's Caddy is downloaded per
  release (`<state>/ingress/bin/caddy-<version>`) and checked against the
  digest compiled into isb. The build tools image is named after a hash of
  its recipe, so a release that changes the recipe prepares a new
  `isb-builder/<hash>` image at the next build; delete old ones with
  `incus image delete` ([Builds](../guides/builds.md#the-builder-image)).

## On a Mac

`isb machine init` installs a Linux isb into the VM once, as
`/usr/local/bin/isb` with the unit `isb.service`; `isb machine start` does not
replace it. The Mac's own isb is upgraded like any binary, and the LaunchAgent
runs isb by its full path, so rerun `isb serve install` after moving or
upgrading it.

To upgrade the daemon in the VM, put the new Linux build somewhere under your
home (the VM sees it at the same path) and install it there:

```sh
isb machine ssh -- sudo install -m 0755 ~/Downloads/isb /usr/local/bin/isb
isb machine ssh -- sudo systemctl restart isb
```

The binary must be a `*-unknown-linux-musl` build for the Mac's architecture
(the release asset `isb-vX.Y.Z-<arch>-unknown-linux-musl.tar.gz`).
`isb machine rm` followed by `isb machine init` also gets the current
release, but deletes everything in the VM.

## Servers and dedicated VMs

A control plane installs isb on each server once, when the server is added
([Servers](../guides/servers.md#adding-a-server)): by default this version's
release, checked against its `SHA256SUMS`, at `/usr/local/bin/isb`, run by
`isb-agent.service`. A dedicated VM gets a copy of the control plane's own
executable when it is created. Neither is upgraded when the control plane is,
and `isb server add` refuses a name that is already recorded, so it does not
reinstall an existing server.

`isb server ls` shows each server's isb version from its heartbeat. To upgrade
an agent, replace the binary on the box and restart the agent:

```sh
# on the server (or, for a dedicated VM: incus exec --project isb-system vm-ORG -- ...)
sudo install -m 0755 ./isb /usr/local/bin/isb
sudo systemctl restart isb-agent
```

While an agent restarts, its orgs' workloads keep running and calls for them
fail with "reach server NAME"; the control plane follows the agent's new event
feed from the start once it answers again.
