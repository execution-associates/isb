# Orgs: the trust boundary

An org holds its people's and agents' sandboxes, stacks, volumes and secrets.
Everything in an org is administered by its members, and nothing crosses
orgs. Every command, daemon tool and stack takes `--org ORG`; without it you
work in the `default` org.

```text
isb org create NAME [--cpus N] [--memory 16GiB] [--disk 100GiB] [--instances N]
                    [--default-cpus N] [--default-memory 512MiB]
                    [--bind-root DIR]... [--allow-egress DEST]...
isb org ls [--json]
isb org show NAME [--json]
isb org rm NAME [--force]
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run]
```

`isb org create` on an existing org updates it to the flags given.

## From the API and the web UI

The same operations are daemon tools ([serve.md](serve.md#tools)), and the
web UI's org Settings and Platform pages use them:

| Tool | Who | Does |
|---|---|---|
| `org_get` | the org's members | limits, defaults, network, egress, bind roots, service-name domain (`<org>.isb`), counts |
| `org_list` | platform admins | every org |
| `org_create` | platform admins | `isb org create` without `--bind-root` |
| `org_update` | platform admins | limits, per-instance defaults, egress exceptions |
| `org_delete` | platform admins | `isb org rm`, refused while stacks are deployed in the org |

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

Org `acme` is the incus project `isb-acme` (config `user.isb.org=acme`), its
bridge `isbbr<hash>` and its network ACL `isb-acme`. The `default` org is incus'
`default` project: everything that predates orgs keeps working there, without
any of what follows.

The project is **restricted**, so incus itself refuses what would reach the
host:

- unprivileged containers only; no nesting, no `raw.lxc`, no `raw.idmap` of
  root, no proxy devices;
- disks are managed volumes only, or bind mounts from the org's
  `--bind-root` directories;
- the org's own bridge is the only network;
- each instance gets its own uid range (`security.idmap.isolated`), and only
  the daemon's own uid may be mapped 1:1 (so `idmap: auto` keeps bind-mounted
  files writable).

`--cpus`, `--memory`, `--disk` and `--instances` limit the org as a whole.
Once a project has limits, incus wants limits on every instance, so the org's
default profile carries `--default-cpus` (1) and `--default-memory` (512MiB)
for instances whose spec sets none. The host's images are shared with every
org.

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

Traffic into an org's instances is not filtered by the ACL: the host, and the
daemon's load balancer, reach them.

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
destination are not filtered. An exception outside the private ranges changes
nothing, since the internet is allowed anyway.

An exception only lifts the org's ACL. The host's firewall still applies: a
destination routed out of an interface other than the uplink (a tailnet host
through `tailscale0`, say) also needs the host to forward to it.

### Host firewall: `isb host setup`

A host with a default-deny firewall (ufw) drops DHCP, DNS and forwarding on new
bridges. `sudo isb host setup` once lets every org bridge (`isbbr+`) through:

- DHCP to the host, accepted ahead of ufw's conntrack checks (a block in
  `/etc/ufw/before.rules`, backed up first);
- DNS to the host (`ufw allow in on isbbr+ to any port 53`);
- egress through the uplink (`ufw route allow in on isbbr+ out on <uplink>`).

ufw's routed default-deny then keeps org bridges apart from each other and
from the host's other networks. It also creates the service-name directory
(below). `--dry-run` prints all of it instead; without root, it prints it and
exits 1.

## Service names

Inside an org, a stack's service is reachable at a stable name:
`<service>.<stack>.<org>.isb`, and `<service>.<stack>` for short. It resolves
to every replica in rotation and follows rollouts; see
[stacks.md](stacks.md#service-discovery).

How it works: the org's bridge runs incus' dnsmasq with `raw.dnsmasq` set to
`hostsdir=/var/lib/isb/dns/<org>`. dnsmasq watches that directory and re-reads
a file as soon as one is renamed into it, so the daemon keeps one hosts file
per service there and DNS follows within milliseconds, with no dnsmasq restart.
`raw.dnsmasq` is set once, when the org is created: changing it later restarts
the org's dnsmasq.

- `sudo isb host setup` creates `/var/lib/isb/dns`, owned by the user isb runs
  as (`--user`, default the one who ran sudo) with group `incus` (dnsmasq's)
  and the setgid bit, mode 2750: the daemon writes, dnsmasq reads, nobody else
  can. A host without an `incus` group gets a world-readable 0755 directory.
- `isb org create` makes `<org>/` in it. On a host without the directory the
  org is created without service names and says so; run `sudo isb host setup`
  and then `isb org create` again (that sets `raw.dnsmasq`, restarting the
  org's dnsmasq once).
- `isb org rm` deletes the org's directory.
- `ISB_DNS_DIR` moves the directory, for `isb org` and `isb serve` alike.

Trade-off: incus runs a bridge's dnsmasq unconfined (no AppArmor profile) once
`raw.dnsmasq` is set, and it is the only way to point dnsmasq at a directory.
dnsmasq still drops to the `incus` user.

## Removing an org

`isb org rm NAME` refuses an org that has instances; `--force` deletes them,
then the project (with its volumes, profiles and buckets), the bridge, the ACL
and the service-name directory.
