---
title: Orgs
description: The org is isb's trust boundary, an incus project with its own network, quotas and egress rules.
order: 2
---

An org holds its people's and agents' sandboxes, stacks, volumes and secrets.
Everything in an org is administered by its members, and nothing crosses
orgs. That makes the org the unit you hand to a team, a customer or an agent:
give them an org, and they can do anything inside it and nothing outside it.

Every command, daemon tool and stack takes `--org ORG` (or `$ISB_ORG`);
without it you work in the `default` org.

```text
isb org create NAME [--cpus N] [--memory 16GiB] [--disk 100GiB] [--instances N]
                    [--default-cpus N] [--default-memory 512MiB]
                    [--bind-root DIR]... [--allow-egress DEST]...
                    [--allow-domain SUFFIX]... [--ingress caddy|cloudflare-tunnel]
                    [--cloudflare-account ID] [--cloudflare-zone ID]
                    [--server SERVER | --vm [--vm-cpus N] [--vm-memory 4GiB] [--vm-disk 40GiB]]
isb org ls [--json]
isb org show NAME [--json]
isb org rm NAME [--force] [--delete-vm]
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run] [--public-ingress]
```

`isb org create` on an existing org updates it to the flags given. `system`
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
| `org_get` | the org's members | limits, defaults, network, egress, bind roots, service-name domain (`<org>.isb`), counts, `placement` |
| `org_list` | platform admins | every org, each with the `server` it runs on (`local` for this daemon) and its `placement` |
| `org_create` | platform admins | `isb org create` without `--bind-root`; `placement` puts it on a server or in a dedicated VM |
| `org_update` | platform admins | limits, per-instance defaults, egress exceptions (a different `server` or `placement` is refused) |
| `org_delete` | platform admins | `isb org rm`, refused while stacks are deployed in the org, and while it has sandboxes unless `force`; `delete_vm` also deletes a dedicated VM |

Limits and egress exceptions are what keep one org from the others and from
the host's networks, so changing them is for platform admins, not the org's
own owners and admins, who see them read-only. Bind roots are host paths and
are set only on the host (`isb org create --bind-root`): an update through
the API keeps them, as it keeps any field it is not given. A limit, once
set, can be changed but not lifted, as with the CLI. Creating an org through
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

The `default` org depends on the host:

- **A fresh host** (incus' `default` project holds no instances, and isb has
  no default-org stacks or apps when `isb serve` first starts): the daemon
  makes it a real org, the incus project `isb-default` with its own bridge,
  ACL and service names, like any other. Multi-app templates work there.
  incus' own `default` project is then not an org: plain `isb create` and
  `isb up` (no `--org`) still put sandboxes there, and the TUI still shows
  them to local callers.
- **A host whose incus `default` project already held workloads:** the
  `default` org *is* incus' `default` project, so everything that was there
  keeps working where it is, without any of what follows: no restrictions,
  no org network, no service names and no workspace. Apps that reach each
  other by name (multi-app templates) need another org there.

`isb org ls` shows which: the default org's project is `isb-default` or
`default`.

The project is **restricted**, so incus itself refuses what would reach the
host:

- unprivileged containers only; no nesting, no `raw.lxc`, no `raw.idmap` of
  root, no proxy devices;
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
with every org. When an instance or volume would pass a limit, isb says
which (`org lab is at its CPU quota (limits.cpu 2, 2 in use)`) and how to
raise it: `isb org create lab --cpus N` on the host, or `org_update`.

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
  org is created without service names and says so; run `sudo isb host
  setup` and then `isb org create` again (that sets `raw.dnsmasq`,
  restarting the org's dnsmasq once).
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
