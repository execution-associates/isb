---
title: Sandboxes and reconciling
description: What a sandbox is, why isb builds on incus, and how isb makes incus match what you described.
order: 1
nav_title: Sandboxes
---

A sandbox is one incus instance that isb creates from a description and then
keeps matching it. You describe it in an `isb.yaml` file (or in Python,
TypeScript or Rust), and every `isb up` compares the description with what
exists and changes only the difference. This page explains the parts that
make that dependable; the field-by-field detail is in the
[isb.yaml reference](../reference/compose.md).

## Why incus

[incus](https://linuxcontainers.org/incus/) runs **system containers**: a
whole Linux machine, with its own init, users, services and network, rather
than a single process. That makes it a natural fit for sandboxes that people
and agents actually work in.

- **Feels like a VM, starts like a container.** A container is usable in a
  few seconds (about 5 on a busy host, image cached) and costs almost nothing
  when idle. Leave it running.
- **VMs with the same tool.** Change `type: container` to `type: vm` when you
  want a separate kernel between the code and your machine. Same file, same
  commands, same API.
- **Unprivileged by default.** Root inside a container is an ordinary user
  outside it, in its own user namespace. Mount only the directories a sandbox
  needs.
- **Real networking.** Every sandbox gets its own address on a private bridge,
  plus port forwards in either direction: publish a guest port on the host, or
  let the guest reach one host service and nothing else.
- **Yours.** It runs on any Linux box you control, laptop to server, with no
  account, no cloud and no per-minute bill. incus is open source (Apache 2.0)
  and maintained by the Linux Containers project. On a Mac, isb runs incus in
  a Linux VM it manages ([isb on macOS](../getting-started/macos.md)).

## What isb adds

incus has the machinery. isb makes it declarative and dependable:

- **Converges, never churns.** `isb up` compares what you asked for with what
  exists and changes only the difference. A mount that is already right is
  never touched, so a dev server's file watching (hot reload) keeps working
  through every `up`. `isb plan` shows the difference first.
- **Nothing hangs silently.** Every call to incus has a deadline, and a stall
  is reported as the step that stalled ("create instance web stalled after
  600s"), not a terminal that sits there.
- **`exec` that behaves.** Arguments stay a list, never glued into a shell
  string. Output streams as it is produced. Exit codes, stdin, a real
  terminal when you have one, and Ctrl-C all work, and a command that does
  not read stdin never waits for it.
- **Nothing left running by accident.** A foreground `isb up` notices when
  the process that started it exits, even when no signal arrives (an agent's
  background task, a closed terminal), and stops its sandboxes.
- **Ready means ready.** Wait for the network, a user, a writable path or
  your own check before the first command, not just for "running".
- **Made for many sandboxes at once.** Labels to find them, `prune` to delete
  the ones whose project directory is gone, and a lock so two tasks never
  create the same one twice.
- **One engine, four ways in.** The CLI, Rust, Python and TypeScript all
  drive the same core, so they behave identically
  ([isb from code](../guides/sdk.md)). It ships as a single static binary.

## Containers and VMs

| | `type: container` (default) | `type: vm` |
|---|---|---|
| Kernel | shares the host's | its own (qemu) |
| Boot | about a second after Running | tens of seconds (50 to 90 s under nested virtualization) |
| Image | any image | a VM image, such as `images:ubuntu/24.04/cloud` |
| Default readiness | `[running]`, 60 s | `[running, agent]`, 300 s: exec goes through the incus agent |
| Bind mounts | idmapped, inotify works | virtiofs: host-side edits do not reach file watchers inside (use polling) |
| Ports | either direction | host-bound only, through incus' NAT mode |
| `privileged`, custom `idmap` | allowed | errors |

A container is the everyday choice. Use a VM when the code is risky or
untrusted: a kernel exploit escapes a container, not a VM. The full list of
VM rules, including NAT ports and their interaction with a host firewall, is
in [Containers vs virtual machines](../reference/compose.md#containers-vs-virtual-machines).

## How reconciling works

`isb plan` and `isb up` resolve each sandbox against the host (storage pools,
subordinate ids, path translation), read the instance, and diff:

- **Creating** makes missing named volumes, then the instance in one request
  with all its config and devices, so every mount, label and idmap exists
  before first boot. Then it starts it, adds searched ports and fixes the
  ownership of mounts that ask for it.
- **Config keys** the file produces are set when their value differs. Keys not
  in the file are never removed, so removing a field does not unset it.
- **Devices** that are already correct are never touched: re-adding a disk
  remounts it, which silently kills a running dev server's inotify watches. A
  wrong device is replaced; a device the file does not mention is left alone
  unless `--prune-devices`.
- **Fixed at creation:** the image, the instance type, the root storage pool
  and the incus profiles. `plan` reports drift in them as a note; recreate
  (`isb down`, then `isb up`) to change them.
- isb never restarts an instance on its own. A changed `raw.*` or
  `security.*` key is reported as taking effect after `isb restart`.

Plan lines are `+` create or add, `~` change, replace or chown, `-` remove,
`>` start, and `note:`. `isb plan --exit-code` exits 2 when there is a change.

Each `isb up` holds a per-sandbox lock, so two tasks never create the same
sandbox twice. Updates are read-modify-write guarded by `If-Match`, so a
concurrent change by another tool causes a re-read and retry, never a silent
overwrite. If a create times out, isb deletes the half-created instance only
when it carries the random token this call wrote, so it never deletes someone
else's. Details: [How reconcile works](../reference/compose.md#how-reconcile-works)
and [The ensure flow](../reference/compose.md#the-ensure-flow).

## Network

A sandbox gets an address on a private bridge and, unless you say otherwise,
reaches the internet. For code you do not trust, `egress:` confines it to a
list of hostnames (or to nothing) behind a proxy `isb serve` runs, and keeps
secrets out of the guest, for containers and VMs alike
([Sandbox egress and secrets](../guides/egress.md)).

## Readiness

"Running" alone is not ready: networking comes up a moment after the
instance does. A sandbox's `ready` list is checked in order after `isb up`
and `isb create`, retried every 250 ms until a shared deadline:

| Check | Passes when |
|---|---|
| `running` | incus reports the instance Running |
| `agent` | a command runs through the incus agent (a VM's agent answers exec) |
| `default_route` | the guest has a default route |
| `{user_exists: U}` | `getent passwd U` succeeds |
| `{path_writable: P}` | `P` is writable by the service's user |
| `{command: [...]}` | your own argv exits 0 |

An instance that stops while getting ready is started once more, then
readiness fails at once rather than at the deadline. See
[`ready`](../reference/compose.md#ready).

## The foreground `up`

Like `docker compose up`, `isb up` stays in the foreground: it runs each
service's `command`, streams its output with a `<service> | ` prefix, and
stops the sandboxes when the first of these happens:

| Event | Exit code |
|---|---|
| every `command` has exited | the first non-zero status, else 0 |
| SIGINT, SIGTERM or SIGHUP | 128 + signal (130 for Ctrl-C) |
| a process that started isb exits | 129 |
| stdout is closed | 141 |

The third is the one signals cannot give: a closed terminal, or an agent
whose background task ends with it, does not always signal its descendants.
isb records its ancestors at startup and checks them every second, so a dev
server never outlives the session that wanted it. That includes a wrapper
script that backgrounds `isb up` and exits: use `isb up -d` for that, which
creates or reconciles, waits until ready and returns.

Stopping is not deleting: the next `isb up` starts the sandboxes again with
their state, and `isb down` deletes them. See
[Foreground `up`](../reference/compose.md#foreground-up).

## Services that outlive `isb up`

A service with `restart: always` (or `on-failure`, `unless-stopped`) is
supervised inside its guest instead: on a system image its `command` becomes
the systemd unit `isb-<service>.service`, on an OCI image it is the
instance's own process, and the instance starts with the host. `isb up -d`
then leaves an app running with nothing of isb's running. For replicas, a
load balancer and rolling updates, deploy the same file as a
[stack](stacks.md). See [`restart`](../reference/compose.md#restart).

## Finding and cleaning up

- **Labels** (`labels: {owner: my-task}`) become `user.<key>` config, and
  `isb ls --label owner=my-task` finds them.
- **`isb prune --label KEY --missing-path`** deletes sandboxes whose `KEY`
  label names a host path that no longer exists (a deleted worktree). It is a
  dry run unless given `-y`.
- Sandboxes made by `isb create` and `isb up` on the host have no expiry.
  Sandboxes made through the daemon do: see
  [Workspaces and sandboxes](workspaces.md#sandboxes-are-short-lived).
