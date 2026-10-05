---
title: Orgs
description: The org is isb's trust boundary, an incus project with its own network, quotas and egress rules.
order: 2
---

An org holds its people's and agents' sandboxes, stacks, volumes and secrets.
Everything in an org is administered by its members, and nothing crosses
orgs. That makes the org the unit you hand to a team, a customer or an agent:
give them an org, and they can do anything inside it and nothing outside it.

Every command, daemon tool and stack takes `--org ORG` (or `$ISB_ORG`).
Platform commands (`stack`, `app`, `project`, `secret`, ...) without it work
in the `default` org; plain sandbox commands (`isb create`, `isb up`) without
it work outside every org ([below](#what-an-org-is-in-incus)).

```text
isb org create NAME [--cpus N] [--memory 16GiB] [--disk 100GiB] [--instances N]
                    [--default-cpus N] [--default-memory 512MiB]
                    [--bind-root DIR]... [--allow-egress DEST]...
                    [--allow-domain SUFFIX]... [--ingress caddy|cloudflare-tunnel]
                    [--cloudflare-account ID] [--cloudflare-zone ID]
                    [--server SERVER | --vm [--vm-cpus N] [--vm-memory 4GiB] [--vm-disk 40GiB]]
isb org update NAME [--cpus N|none] [--memory SIZE|none] [--disk SIZE|none] [--instances N|none]
                    [--default-cpus N] [--default-memory SIZE]
                    [--allow-egress DEST]... [--allow-udp IP:PORT]...
isb org ls [--json]
isb org show NAME [--json]
isb org rm NAME [--force] [--delete-vm]
isb org nesting NAME [on|off]
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run] [--public-ingress]
```

`isb org create` on an existing org sets the flags given. `isb org update`
changes an existing org through the daemon: flags left out keep their value,
and `none` lifts a limit (`isb org update lab --disk none`). `system`
is not an org name: the incus project `isb-system` holds isb's own services
(the local registry, dedicated VMs).

Where an org runs is chosen once, when it is created
([Placement](placement.md)):

- by default, on this host;
- `--server SERVER` creates it on another host instead, one the local daemon
  (a control plane) added with `isb server add`
  ([Servers and dedicated VMs](../guides/servers.md)). The org's project,
  network and workloads then live on that server, and every call for it goes
  there; `isb org show` and `isb org rm` find it through the daemon;
- `--vm` runs it in a **dedicated VM**: the local daemon makes a VM on this
  host for the org alone, with its own kernel and its own incus, registers it
  as server `vm-NAME` and places the org there. `isb org rm NAME --delete-vm`
  deletes the VM with the org.

An org does not move between placements afterwards
([Moving an org](placement.md#moving-an-org)).

An org's builds ([Builds and the local registry](../guides/builds.md)) run in
its own project too, as ordinary unprivileged containers (or VMs for
untrusted source) that count against its quota, and its images are the
registry repositories `<org>/*`, which only its own stacks and sandboxes can
name.

## From the API and the web UI

The same operations are daemon tools ([MCP tools](../reference/mcp-tools.md)),
and the web UI's org Settings and Platform pages use them:

| Tool | Who | Does |
|---|---|---|
| `org_get` | the org's members | limits with what is allocated against each (`allocation`), defaults, network, egress, bind roots, service-name domain (`<org>.isb`), counts, `placement` |
| `org_list` | platform admins | every org, each with the `server` it runs on (`local` for this daemon) and its `placement` |
| `org_create` | platform admins | `isb org create` without `--bind-root`; `placement` puts it on a server or in a dedicated VM |
| `org_update` | platform admins | `isb org update`: limits (`"none"` or `null` lifts one), per-instance defaults, egress exceptions (a different `server` or `placement` is refused) |
| `org_delete` | platform admins | `isb org rm`, refused while stacks are deployed in the org, and while it has sandboxes unless `force`; `delete_vm` also deletes a dedicated VM |
| `org_nesting` | superadmins | whether the org's workspace may run Docker (`isb org nesting ORG on\|off`); `org_get` shows it as `allow_nesting` |

Limits and egress exceptions are what keep one org from the others and from
the host's networks, so changing them is for platform admins, not the org's
own owners and admins, who see them read-only. Bind roots are host paths and
are set only on the host (`isb org create --bind-root`): an update through
the API keeps them, as it keeps any field it is not given. Creating an org through
the API also adds it to the identity store, and deleting one removes its
memberships, invitations and tokens; its secrets stay under the state
directory.

## What an org is in incus

An org on this host shares the host's kernel with every other local org: the
project, bridge and ACL below keep them apart, and the kernel is what they
all trust. `org_get` says so as `placement.isolation`: `shared-kernel` here,
`own-host` on a server, `own-kernel` in a dedicated VM
([Placement](placement.md)).

Org `acme` is the incus project `isb-acme` (config `user.isb.org=acme`), its
bridge `isbbr<hash>` and its network ACL `isb-acme`.

The `default` org is an org like any other: the incus project
`isb-default`, with its own bridge, ACL and service names, settings, limits,
egress rules, workspaces and, if you set one, a Cloudflare Tunnel
([Domains](../guides/domains.md)). Multi-app templates and workspaces work in
it as in any org. `isb serve` makes sure it exists on every start, so
`isb serve install`, which starts the daemon, leaves a host with one, and it
cannot be removed. `isb org ls` shows it as `default -> isb-default`.

incus' own `default` project is never an org. Plain `isb create` and `isb up`
without `--org` put sandboxes there, outside every org, and the TUI and the
superadmin's Host page show them; `--org default` means the default org,
`isb-default`.

If a default-org stack still has instances in incus' `default` project,
`isb serve` logs a warning when it starts: the controller recreates that
stack in `isb-default` with new, empty volumes, and the old instances and
volumes keep running in `default` until you delete them. isb has no tool to
move data between the two projects.

The project is **restricted**, so incus itself refuses what would reach the
host:

- unprivileged containers only; no nesting, no `raw.lxc`, no `raw.idmap` of
  root, no proxy devices (a platform admin can allow a stack's UDP ports,
  [UDP ports](#udp-ports); a superadmin can allow nesting for the org's
  workspace alone: [The Docker exception](security.md#the-docker-exception));
- disks are managed volumes only, or bind mounts from the org's
  `--bind-root` directories;
- the org's own bridge is the only network;
- each instance gets its own uid range (`security.idmap.isolated`), and only
  the daemon's own uid may be mapped 1:1 (so `idmap: auto` keeps
  bind-mounted files writable);
- snapshots and exports are allowed (`restricted.snapshots`,
  `restricted.backups`): isb takes them for
  [volumes](../guides/volumes.md).

`--cpus`, `--memory`, `--disk` and `--instances` limit the org as a whole.
Once a project has limits, incus wants limits on every instance, so the
org's default profile carries `--default-cpus` (1) and `--default-memory`
(512MiB) for instances whose spec sets none. The host's images are shared
with every org.

### Limits are budgets

incus enforces an org's limits as budgets of what is **allocated**, not of
what is used: `limits.cpu` caps the sum of every instance's `limits.cpu`,
`limits.memory` the sum of their `limits.memory`, and `limits.disk` the sum
of their root disks' `size` and the org's volumes' sizes. Every instance in
the org counts, stopped ones included, so stopping an instance frees
nothing; deleting it or lowering its limits does. `--instances` counts
instances the same way. Nothing caps what the org's instances use together
at any moment: that would need a parent cgroup per project, which incus does
not offer, so isb has no shared-ceiling mode.

For example, an org with `--cpus 4 --memory 4GiB` and the defaults (1 CPU,
512MiB) fits four instances that set no limits, running or stopped, and a
fifth is refused even if the four are idle. Instead of those four, one stack
service with `cpus: 2` and two replicas takes all four CPUs, and 2 x 512MiB
of the memory (or two of what the service sets).

`isb org show` prints each limit with its allocation:

```text
instances  3 of 5, stopped ones included
cpus       3 of 4 allocated, 1 free
memory     1.5GiB of 4GiB allocated, 2.5GiB free
disk       unlimited
           (allocated: the sum of every instance's limit, stopped ones included)
defaults   1 CPU, 512MiB memory per instance whose spec sets none
```

`org_get` and `isb org show --json` have the same as `allocation`
(`{"cpu": {"limit": 4, "allocated": 3, "free": 1}, ...}`, bytes for memory
and disk; a limit that is not set is left out). When an instance or volume
would pass a limit, isb says which, what is allocated, and what the request
needed (`org lab is at its CPU quota (limits.cpu: allocated 4 of 4, the sum
of every instance's limit, stopped ones included; web-3 needs 1 (the org's
default))`), and how to raise or lift it: `isb org update lab --cpus N` (or
`--cpus none`) on the host, or `org_update`.

**Disk.** Under `limits.disk` incus refuses an instance whose root disk has
no `size`. isb gives every instance it creates in such an org a size of its
own, in the create request: the service's `raw_devices.root.size`
(`raw_devices: {root: {size: 20GiB}}`, allowed for remote callers without
`--allow-raw`, since it is only a quota), else 10GiB. That covers stack
replicas, job runs, builds and sandboxes; workspaces get 20GiB. The size is
not part of a stack service's revision, so setting or lifting a limit never
rolls a service, and an existing instance is never resized. isb puts no size
on the org's default profile: incus applies a profile's size to every
instance that takes its root disk from the profile. Custom volumes count too:
a named volume whose `config` sets no `size` is created with 10GiB, and a
build's cache volume with the build cache size (`ISB_BUILD_CACHE_SIZE`,
default 20GiB).

Setting a disk limit (`isb org update ORG --disk 100GiB`, `org_update`) on an
org that has instances without a root size is refused up front, naming each
and what it holds: give its service `raw_devices: {root: {size: ...}}` (at
least what it holds) and redeploy it, or delete it, then set the limit.
`--disk none` lifts the limit; instances keep their sizes.

A root size of exactly 10GiB on the default profile is taken to be isb's
own and comes off at the next `org update`, unless the org has a disk limit
and some instance has no root disk of its own (it takes the profile's):
then it stays, and the update says which instances. Give each a
size (`incus config device override NAME root size=10GiB --project
isb-ORG`) and run the update again, or remove it by hand with
`incus profile device unset default root size --project isb-ORG` once no
limit is set. Any other size on the profile is an operator's and stays.

The project's bind paths are the `--bind-root` directories plus the host
folders of the org's workspace homes, which are recorded on the project, so
rewriting the bind roots never takes a home away.

## The network

Each org has its own bridge with a /24 that incus picks, NAT to the internet,
no IPv6, and DNS domain `<org>.isb`: an instance `web` is `web.acme.isb` to
the others in the org (and plain `web`, through the search domain).

The org's ACL decides where its instances may go:

- **within the org:** anything;
- **the internet:** anything;
- **private ranges** (10/8, 172.16/12, 192.168/16, 100.64/10, 169.254/16):
  rejected, apart from the org's own subnet (and the exceptions below). This
  covers other orgs' bridges, the host's other networks, the LAN and the
  tailnet.

Traffic into an org's instances is not filtered by the ACL: the host, and
the daemon's load balancer, reach them.

A host with a default-deny firewall (ufw) needs `sudo isb host setup` once,
so org bridges get DHCP, DNS and egress while staying apart from each other:
see [Host firewall](../operations/host-setup.md#host-firewall).

### Egress exceptions

`--allow-egress DEST` lets the org reach a private destination anyway, such
as a tailnet host or another org's published service. `DEST` is
`CIDR[:PORTS[/tcp|udp]]`:

| `DEST` | Allows |
|---|---|
| `10.20.0.0/16` | everything to that network |
| `100.79.171.47` | everything to that address (a bare address is a /32) |
| `100.79.171.47/32:1080/tcp` | TCP port 1080 only (`tcp` is the default) |
| `10.1.2.3:53/udp` | UDP port 53 only |
| `10.1.2.3:8000-8100,9000` | those TCP ports |

The flag repeats. Giving it replaces the org's exceptions; leaving it out
keeps them; `--allow-egress none` clears them. They are stored in the
project's `user.isb.egress` and shown by `isb org show`. Two exceptions for
different networks may not overlap; the same network may be listed more than
once (its ports add up, and a whole-network exception wins).

How they are rendered: incus evaluates an ACL's `drop` rules first, then
`reject`, then `allow`, whatever the order they were written in, so an allow
rule can never punch through the private-range reject. Instead, the
exception's network is carved out of the rejected ranges, as the org's own
subnet is. A port-limited exception then adds reject rules for the rest of
that network: its other TCP ports, its other UDP ports (all of them if none
were allowed) and ICMP. Other IP protocols (GRE, SCTP, ...) to a port-limited
destination are not filtered. An exception outside the private ranges
changes nothing, since the internet is allowed anyway.

An exception only lifts the org's ACL. The host's firewall still applies: a
destination routed out of an interface other than the uplink (a tailnet host
through `tailscale0`, say) also needs the host to forward to it.

## UDP ports

A stack's UDP port is a NAT proxy device on its replica
([UDP ports](stacks.md#udp-ports)) and takes that port on that host address
away from everyone else, so the org does not pick its own: a platform admin
lists the ports it may publish, `IP:PORT` each, with a specific host address.

```sh
isb org create media --allow-udp 203.0.113.7:10000 --allow-udp 203.0.113.7:59000
```

The flag repeats. Giving it replaces the list; leaving it out keeps it;
`--allow-udp none` clears it. Over the tools it is `org_update`'s `udp`. The
list is stored in the project's `user.isb.udp` and shown by `isb org show`. A
stack deploy that publishes UDP anywhere else is refused, whoever deploys it.
Taking a port off the list does not touch a stack already publishing it; its
next deploy is refused.

Two layers keep proxy devices to those ports, as for [the Docker
exception](security.md#the-docker-exception):

- **The project.** With a UDP port listed, the org's project allows proxy
  devices (`restricted.devices.proxy=allow`); with none, it blocks them
  again. Clearing the list fails (incus refuses) while a replica still has
  one: remove the stack first.
- **isb itself.** Once the project allows them, isb refuses every proxy
  device in every instance of an org project except a stack replica's UDP
  port: host-bound, NAT mode, UDP into the instance's own address, made by
  the stack controller, whose mark no spec, compose file or tool argument
  can carry. A sandbox's `ports`, a guest-bound port and a raw proxy device
  are refused, whoever asks.

## Domains

The hostnames an org's stacks may serve through the ingress
([Domains and ingress](../guides/domains.md)) are the platform's to decide:

- `--allow-domain SUFFIX` (repeatable) limits them to names at or under each
  suffix: `example.com` allows `example.com` and `shop.example.com`.
  `*.example.com` also allows wildcard hosts (`*.example.com`,
  `*.team.example.com`). Giving the flag replaces the list; `none` clears
  it. Stored in the project's `user.isb.domains`.
- Without a list, any concrete name is allowed and no wildcard.
- Generated names (`host: auto`, under sslip.io) are always allowed.
- Whatever the lists say, a name one org serves is refused to every other:
  the first to claim it keeps it.

`--ingress` picks how the org's domains are reached: `caddy` (default) on
the server's public listeners, or `cloudflare-tunnel` through the org's own
Cloudflare Tunnel, whose token the org keeps in its secret
`cloudflare-tunnel-token`. With an API token in `cloudflare-api-token` as
well, isb manages the tunnel's ingress rules and the hostnames' DNS records;
`--cloudflare-account` and `--cloudflare-zone` name the account (default:
the tunnel token's) and zone (default: looked up per hostname). Stored in
`user.isb.ingress` and `user.isb.ingress.cloudflare.*`.

## Service names

Inside an org, a stack's service is reachable at a stable name:
`<service>.<stack>.<org>.isb`, and `<service>.<stack>` for short. It
resolves to every replica in rotation and follows rollouts; see
[Service discovery](stacks.md#service-discovery).

How it works: the org's bridge runs incus' dnsmasq with `raw.dnsmasq` set to
`hostsdir=/var/lib/isb/dns/<org>`. dnsmasq watches that directory and
re-reads a file as soon as one is renamed into it, so the daemon keeps one
hosts file per service there and DNS follows within milliseconds, with no
dnsmasq restart. `raw.dnsmasq` is set once, when the org is created:
changing it later restarts the org's dnsmasq.

- `sudo isb host setup` creates `/var/lib/isb/dns`, owned by the user isb
  runs as (`--user`, default the one who ran sudo) with group `incus`
  (dnsmasq's) and the setgid bit, mode 2750: the daemon writes, dnsmasq
  reads, nobody else can. A host without an `incus` group gets a
  world-readable 0755 directory.
- `isb org create` makes `<org>/` in it. On a host without the directory the
  org is created without service names and says so. Once `sudo isb host
  setup` has made the directory, a running `isb serve` sets `raw.dnsmasq` on
  every org that lacks it (restarting that org's dnsmasq once), the `default`
  org included, at its next start or within a minute; with no daemon, run
  `isb org create ORG` again.
- `isb org rm` deletes the org's directory.
- `ISB_DNS_DIR` moves the directory, for `isb org` and `isb serve` alike.
- When the directory is inside the daemon's state directory (a daemon
  running as root keeps its state in `/var/lib/isb`, as does a server's
  agent), `isb serve` makes the state directories on the way traversable
  (mode 0711: others may pass through, not list) so dnsmasq can reach the
  hosts files; everything in them stays 0600/0700.

Trade-off: incus runs a bridge's dnsmasq unconfined (no AppArmor profile)
once `raw.dnsmasq` is set, and it is the only way to point dnsmasq at a
directory. dnsmasq still drops to the `incus` user.

## Removing an org

`isb org rm NAME` refuses an org that has instances; `--force` deletes them,
then the project (with its volumes, profiles and buckets), the bridge, the
ACL and the service-name directory. For an org in a dedicated VM,
`--delete-vm` deletes the VM too.
