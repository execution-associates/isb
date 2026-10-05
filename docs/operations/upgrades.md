---
title: Upgrading isb
description: What to do after installing a new isb binary on a host, on a Mac, and on the servers a control plane manages, and what an upgrade changes on its own.
order: 2
---

isb is one binary, so upgrading is replacing it and then restarting whatever
runs it. A binary from the [installer](../getting-started/install.md#2-the-binary)
updates itself: `isb update` replaces it in place with the latest release,
checked against the release's signed `SHA256SUMS` (`isb update --check`
only reports; see [the CLI reference](../reference/cli.md#updating-isb)). A
source build upgrades with `cargo install isb` again. mise installs are
deprecated, since mise does not check the signature: move to the installer
([Install isb](../getting-started/install.md#2-the-binary)). Workloads keep running
throughout: stopping or restarting the daemon never stops an app.

## On a Linux host

```sh
isb update                          # the latest signed release, in place
systemctl --user restart isb        # the daemon runs the new binary
isb --version
```

- **Restart, or rerun `isb serve install`.** The unit runs the binary that
  installed it by its full path. `isb update` replaces that file in place, so
  a restart runs the new version. A binary at a new path (moving from a mise
  install to the installer, or a source build elsewhere) needs
  `isb serve install` from the new binary: it is idempotent, rewrites the
  unit, keeps `serve.env` and the secrets key, restarts the service and
  waits until it answers.
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

Upgrade the control plane first, then its servers:

```sh
isb server ls                 # ISB says "(differs)" for a server on another build
isb server upgrade --all      # each to the control plane's own build, one after another
```

`isb server upgrade` (the Servers page's **Upgrade** button) sends the
control plane's binary over the agent's mTLS connection (a dedicated VM:
through incus), and a root helper on the box installs it, restarts the
agent and puts the old binary back unless the new agent answers within two
minutes ([Upgrading servers](../guides/servers.md#upgrading-servers)). While an
agent restarts, its orgs' workloads keep running and calls for them fail
with "reach server NAME"; the control plane follows the agent's new event
feed from the start once it answers again. A control plane refuses to
forward to an agent that speaks a newer protocol than it does, so upgrade
the control plane before its servers.

A server added by an isb without `server_upgrade` has no helper (a dedicated
VM gets one with its first upgrade). Replace its binary by hand once:

```sh
# on the server
sudo install -m 0755 ./isb /usr/local/bin/isb
sudo systemctl restart isb-agent
```

Only the bootstrap installs the helper on a server added over SSH, so such a
server is upgraded by hand until it is added again (`isb server rm` once no
org is placed on it, then `isb server add`).
