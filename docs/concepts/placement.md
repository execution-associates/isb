---
title: "Placement: this host, a server, a dedicated VM"
description: Where an org runs decides what keeps it apart from other orgs, from a shared kernel to a machine of its own.
order: 6
nav_title: Placement
---

Every org runs somewhere, and where it runs decides what keeps it apart from
the other orgs. Most orgs run on the host the daemon runs on, as an incus
project of their own. An org that should share nothing with the others can
run on another machine, or in a VM that isb makes for it alone. You choose
once, when the org is created, and the choice is shown wherever the org is.

| Placement | `placement` | Isolation | What shares its kernel |
|---|---|---|---|
| This host | `"local"` (default) | `shared-kernel`: an incus project with its own bridge, network ACL and quotas ([Orgs](orgs.md#what-an-org-is-in-incus)); unprivileged containers | every other local org, and the host |
| A server | `{"server": "hel-1"}` | `own-host`: another machine, added with `isb server add` | the other orgs placed on that server |
| A dedicated VM | `{"vm": {"cpus": 2, "memory": "4GiB", "disk": "40GiB"}}` | `own-kernel`: a VM on this host made for the org alone, with its own incus | nothing |

```sh
isb org create acme                                   # this host
isb org create beta --server hel-1                    # a server added with isb server add
isb org create gamma --vm --vm-cpus 4 --vm-memory 8GiB --vm-disk 100GiB
```

Through the API it is `org_create` with `placement` (`server: "hel-1"` is
accepted too, the same as `placement: {"server": "hel-1"}`); platform admins
only. The web UI's **New org** dialog asks where the org should run, each
choice with what keeps it apart. `org_get` and `org_list` answer
`placement: {kind, server, isolation}`, where `kind` is `local`, `server` or
`vm`.

## Choosing

- **This host** is the default and the cheapest: no extra machine, instant
  creation. Orgs trust the host's kernel, which is what every container on it
  trusts. Good for your own teams and for agents you run.
- **A server** puts the org on another machine that the control plane added
  over SSH. Its workloads, secrets and state live on that machine; a server
  outage affects only the orgs on it. Good for spreading load, putting an org
  near its users, or keeping a customer on hardware of their own.
- **A dedicated VM** gives the org its own kernel on this host, so a kernel
  exploit in one org's container does not reach the others. It costs the
  VM's CPUs, memory and disk, and minutes to create. The host needs KVM:
  incus must list the `qemu` driver and `/dev/kvm` must exist, which a cloud
  VM without nested virtualization does not have. Where it is missing, the
  option is refused with the reason (the web UI shows it disabled).

## How the control plane works

A daemon that places orgs elsewhere is the **control plane**. Each other
machine is a **server** running `isb serve --agent`. People, agents, the CLI
and the web UI only ever talk to the control plane; it authenticates the
caller, checks their role, writes its audit row, and forwards each call for
an org to the server that holds it over mutual TLS. The server's agent then
judges the asserted caller again in that org.

This is federation, not incus clustering. Every server is a complete isb for
the orgs on it (the stack controller, ingress, registry, builds, secrets,
history), so a server keeps running its orgs' workloads when the control
plane is away, and an org never spans servers. A few things stay on the
control plane whatever the org: the identity store, the event feed, the
audit log, template catalogs, `registry_gc`, `notification_settings`,
`server_status` and the `server_*` tools.

Adding servers, the mTLS details, health checks and failure modes are in
[Servers and dedicated VMs](../guides/servers.md).

## Dedicated VMs

A dedicated VM is a server the control plane provisions itself, through
incus rather than SSH: a VM named `vm-<org>` in the `isb-system` project,
from `images:ubuntu/24.04`, with the size you gave (each optional: 2 CPUs,
4GiB of memory, 40GiB of disk; at least 2GiB of memory and 10GiB of disk),
running incus and a copy of the control plane's own isb as its agent. It is
registered as server `vm-<org>` and the org is created on it.

What it means:

- The VM's CPUs, memory and disk are its own incus limits on the host. They
  count against nothing else: not the org's quota (which applies inside the
  VM), not another org's, and isb keeps no host-wide budget. Size it so the
  host can hold it.
- Its agent port is reachable from the host only, and no SSH is installed.
- The org's domains go out through the org's own Cloudflare Tunnel
  (`--ingress cloudflare-tunnel`), since the VM runs no public listeners.
- Its isb is the control plane's build, copied at creation; it is not
  upgraded with the control plane.

Deleting the org with `--delete-vm` (`delete_vm`; a checkbox in the web UI,
on by default there) deletes the VM and forgets the server once the org is
gone; without it the VM keeps running as an empty server, and
`isb server rm vm-<org>` deletes it later. The step-by-step provisioning is
in [Dedicated VMs](../guides/servers.md#dedicated-vms).

## Moving an org

An org does not move after it is created: `org_update` with another `server`
or `placement` is refused, and the web UI says so on the org's Settings page.
To move one by hand:

1. Back up its databases (`isb backup run`) and volumes.
2. Note its apps (`isb app ls`, `isb app show`) and secrets.
3. Remove its stacks and apps, then `isb org rm`.
4. `isb org create NAME --server OTHER` (or `--vm`, or neither for this
   host).
5. Recreate its secrets and apps, and restore its databases
   (`isb backup restore`).
