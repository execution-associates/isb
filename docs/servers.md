# Servers: orgs on other hosts

One `isb serve` can be the **control plane** for others. Each other host is a
**server**: it runs incus and `isb serve --agent`, and the control plane places
whole orgs on it. Users, agents and the web UI only ever talk to the control
plane; it forwards each call for an org to the server that holds the org.

This is federation, not incus clustering. Every server is a complete isb for
the orgs on it (the stack controller, ingress, registry, builds, secrets,
history), so a server keeps running its orgs' workloads when the control
plane is away, and an org never spans servers.

```text
                users, agents, web UI, CLI
                          |
                          |  sessions, API tokens, Access   (authn, authz, audit)
                          v
   +-----------------------------------------------+
   | control plane: isb serve                       |
   |  identity store, web UI, audit log, events     |
   |  orgs placed "local" run here, as ever         |
   |  servers/: CA, servers.json, placement.json    |
   +-----------------------------------------------+
          |  mTLS (client cert from its CA)       ^
          |  POST /orgs/<org>/api/v1/tools/<tool>  |  heartbeat every 10 s
          |  Authorization: IsbAssert <caller>    |  events (long poll), mirrored
          v                                        |
   +--------------------------+     +--------------------------+
   | server "hel-1"            |     | server "nbg-2"           |
   | isb serve --agent :7443  |     | isb serve --agent :7443  |
   |  orgs: acme, beta        |     |  orgs: gamma             |
   |  incus, its own age key, |     |  ...                     |
   |  controller, ingress,    |     |                          |
   |  registry, builds        |     |                          |
   +--------------------------+     +--------------------------+
```

## Placement and isolation

Where an org runs decides what keeps it apart from other orgs. It is chosen
once, when the org is created (`org_create` with `placement`, or the web
UI's New org dialog), and shown wherever the org is (`org_get` and
`org_list` answer `placement: {kind, server, isolation}`):

| Placement | `placement` | Isolation | What shares its kernel |
|---|---|---|---|
| This host | `"local"` (default) | `shared-kernel`: an incus project with its own bridge, network ACL and quotas ([orgs.md](orgs.md#what-an-org-is-in-incus)); unprivileged containers | every other local org, and the host |
| A server | `{"server": "hel-1"}` | `own-host`: another machine, added with `isb server add` | the other orgs placed on that server |
| A dedicated VM | `{"vm": {"cpus": 2, "memory": "4GiB", "disk": "40GiB"}}` | `own-kernel`: a VM on this host made for the org alone, with its own incus | nothing |

An org does not move afterwards ([Moving an org](#moving-an-org)).

## Adding a server

```sh
isb server add hel-1 --ssh root@203.0.113.7 --key ./bootstrap_ed25519 \
    --allow-from 198.51.100.4          # the control plane's egress address
isb server ls
isb server show hel-1
```

`isb server add` (the `server_add` tool; platform admins) needs a fresh Linux
box (Ubuntu or Debian, x86_64 or aarch64) reachable over SSH as root or as a
user with passwordless sudo. Over SSH it:

1. checks the box's architecture (and, for a user other than root,
   passwordless sudo) and uploads the isb binary: `--isb-binary` (a Linux
   build, checked to be one for the box's architecture), `--self-binary`
   (the daemon's own executable: the same build as the control plane), or
   by default this version's release, checked against the release's
   `SHA256SUMS`. The box checks the upload's SHA-256 again before installing it
   at `/usr/local/bin/isb`;
2. creates the system user `isb` (home `/var/lib/isb`), gives the incus
   containers their uid range, installs incus from Zabbly's stable channel
   (as `isb machine` does in its VM) and initialises it (`incus admin init
   --auto`) if it has no storage pool;
3. with `--allow-from`, enables ufw: SSH, and the agent port from those
   addresses only. Without it the port is open to any address (the agent
   still admits only the control plane's client certificate) and the command
   says so;
4. runs `isb host setup --user isb` (org bridges, DHCP, DNS, service names);
5. writes the agent's TLS material to `/etc/isb-agent/` (owned by `isb`,
   0700; key 0600) and the unit `isb-agent.service`, which runs
   `isb serve --agent --agent-listen 0.0.0.0:7443 --agent-tls /etc/isb-agent
   --state-dir /var/lib/isb/state`, and starts it;
6. waits for the agent's heartbeat over mTLS and checks it presents the
   certificate just issued.

Every step is idempotent: rerunning `isb server add` after a failure
converges. The CLI prints each step and the box's output as they happen:
it calls `server_add` with `wait: false`, which answers at once, and
follows `server_provision_get` (steps with their state and times, the log,
the error). The web UI's Add server wizard does the same. A run's progress
stays readable for an hour after it fails (and ten minutes after it
succeeds), in memory only; one run per server name at a time. Without
`wait: false`, `server_add` answers when the server is up, as before.

The SSH key is used for this and never again; the control plane
keeps only `user@host`, the box's host key (`servers/known_hosts`) and what
it dials. SSH runs with exactly that key (`-F /dev/null`, `IdentitiesOnly`,
no agent). Through the API, `key` (a path) is for the local CLI only; a remote
platform admin sends `ssh_key`, the key itself, which is written 0600 for the
bootstrap and deleted after.

| Flag | Default | |
|---|---|---|
| `--ssh user@host` | | how to reach the box |
| `--port` | 22 | SSH port |
| `--key FILE` | | the SSH private key, bootstrap only |
| `--address` | the SSH host | what the control plane dials; the agent certificate's SAN |
| `--agent-port` | 7443 | the agent's mTLS port |
| `--allow-from CIDR` | none | who the box's firewall lets reach the agent port (repeatable) |
| `--isb-binary FILE` | this version's release | the Linux binary to install |
| `--isb-version V` | this version | the release to install |
| `--self-binary` | off | install the daemon's own executable instead (`self_binary`) |
| `--public-ingress` | off | serve the server's orgs' domains on its own 80 and 443 (`isb host setup --public-ingress`, the agent's `--ingress-http`/`--ingress-https`) |

`isb server rm NAME` forgets a server; it is refused while orgs are placed on
it. The agent keeps running on the box until it is stopped there
(`systemctl disable --now isb-agent`); a [dedicated VM](#dedicated-vms) is
deleted with its server. `isb server rotate-cert NAME` issues the
agent a new certificate and key over the current mTLS connection; the agent
writes them and uses them for every new connection, and the control plane
checks that it does.

## mTLS

The control plane has its own CA under `<state>/servers/pki/` (`ca.key` 0600
in a 0700 directory; ten years), apart from the local registry's. It issues:

- a **server certificate** per agent, extended key usage serverAuth only, SAN
  = the address the control plane dials, 397 days;
- one **client certificate** for itself (`CN=isb-control-plane`), clientAuth
  only.

The agent trusts that CA alone and requires a client certificate with the
clientAuth usage, so nothing the control plane did not issue gets a TLS
session, and no agent's server certificate can call another agent. The
control plane trusts the same CA for the agent and checks the agent's address
against the certificate. Both sides use rustls (TLS 1.2/1.3, ring).

The agent's listener is not an API for people: no sessions, no tokens, no web
UI. Each request carries the control plane's assertion of the caller in
`Authorization: IsbAssert <base64url JSON>` (email, how they signed in, the
token's scopes and org, their orgs and roles, platform admin). The agent
believes it only because the connection is the control plane's.

## Placement

Every org is placed `local` (this daemon, the default), on a server, or in
a [dedicated VM](#dedicated-vms), once, at creation:

```sh
isb org create acme --server hel-1 [--cpus 4] [--memory 8GiB] [--allow-egress ...]
isb org create beta --vm [--vm-cpus 4 --vm-memory 8GiB --vm-disk 100GiB]
```

(`org_create` with `placement: {"server": "hel-1"}`, or the older `server:
"hel-1"`; platform admins.) The control plane tells the
agent the org is placed on it, creates it there (its incus project, bridge
and ACL), and adds it to its own identity store for members, invitations and
tokens. The default org is always local. Bind roots, the domain allowlist and
the ingress provider of an org on a server are not set through the control
plane yet.

From then on every org-scoped call for that org goes to its server, on every
surface: MCP (`/mcp` and `/orgs/<org>/mcp`), REST, the CLI over the control
plane's socket, the web terminal's websocket (bridged through to the agent's)
and app webhooks (forwarded as they came; the agent checks the signature,
since it holds the app's secret). The control plane authenticates the caller,
runs its authorizer (roles, token scopes, org scope) and writes its audit row
first; the agent then judges the asserted caller again, in the org the call
was sent for:

- the call goes to `/orgs/<org>/...` on the agent, which pins `org`, so a
  forged `org` in the arguments is refused;
- the agent refuses any org not placed on it, even for a platform admin;
- the asserted principal's role and scopes apply as they would on the
  control plane.

A deploy from the control plane's CLI (`isb stack deploy --org acme`) sends
the resolved compose file as YAML; host paths of the control plane mean
nothing on the server, so its binds are held to the server's own rules
(no bind roots by default).

**Cross-org reads** (`overview`, `stack_list`, `ingress_status`, `org_list`)
run on the control plane and on each server holding an org the caller can
see, in parallel (15 s each), and merge: every item from a server carries
`"server": NAME`, and a server that did not answer is listed under
`unreachable` instead of failing the call. **Events** from servers are
mirrored into the control plane's feed (a long poll per server, only events of
orgs placed on that server), so `events`, `GET /api/v1/events` and the web UI
show them with the control plane's own sequence numbers. A server's feed is
followed from where it is when the control plane starts; what happened while
the control plane was down stays in the server's own history.

Stays on the control plane whatever the org: `events`, `audit_list` and
`audit_verify` (the audit log is the control plane's; agents keep their own
too), template catalogs, `registry_gc`, `notification_settings`,
`server_status` and the `server_*` tools.

### Moving an org

Not supported: `org_update` with another `server` or `placement` is
refused, and the web UI says so on the org's Settings page. To move one by
hand: back up its databases (`isb backup run`), note its apps
(`isb app ls`, `isb app show`) and secrets, remove its stacks and apps,
`isb org rm`, `isb org create NAME --server OTHER` (or `--vm`, or neither
for this host), then recreate its secrets and apps and restore its
databases (`isb backup restore`).

## Dedicated VMs

For an org that should share a kernel with nobody, the control plane makes
it a VM of its own on its own host and runs the org there:

```sh
isb org create acme --vm --vm-cpus 4 --vm-memory 8GiB --vm-disk 100GiB
isb org rm acme --delete-vm
```

`org_create` with `placement: {"vm": {"cpus": 4, "memory": "8GiB", "disk":
"100GiB"}}` (each optional: 2 CPUs, 4GiB, 40GiB; at least 2GiB of memory
and 10GiB of disk), and `wait: false` to answer at once and follow
`server_provision_get` with name `vm-acme`, as the CLI and the web UI do.
The org's own settings (quota, egress) go with it as for any placement and
are checked before the VM is made.

A dedicated VM is just a server the control plane provisioned itself,
through incus rather than SSH:

1. checks the host can run VMs: incus lists the `qemu` driver and
   `/dev/kvm` exists. A host without (a cloud VM without nested
   virtualization, such as Hetzner's cx line) refuses with the reason, and
   the web UI shows the option disabled with it (`server_list` answers
   `dedicated_vm: {supported, reason}`);
2. creates the VM `vm-acme` in the `isb-system` project (made if missing,
   as `isb registry setup` makes it) from `images:ubuntu/24.04`, with
   `limits.cpu`, `limits.memory` and a root disk of that size, on the host's
   managed bridge (`incusbr0`, never an org's), labelled
   `user.isb.dedicated-vm=acme` and `user.isb.server=vm-acme`, and starts it;
3. waits for its guest agent and its address, then reserves that address
   on the NIC (`ipv4.address`), since the control plane dials it and the
   agent's certificate names it;
4. copies the control plane's own isb executable into it (file push) and
   runs the same root script `server add` runs (exec, the script on stdin,
   so the agent's key is never written to the guest's disk but as its TLS
   file): incus from Zabbly, `isb registry setup`, `isb host setup`, the
   agent's TLS material and `isb-agent.service`, and ufw enabled with
   only the agent port open, and only to the host's address on that bridge.
   No SSH is installed or opened;
5. waits for the heartbeat over mTLS, checks the certificate, records server
   `vm-acme` (with a `vm` block: org, project, instance, size), and creates
   the org on it as for any server.

Every step is idempotent: run the same command (or press Retry) after a
failure and it reuses the VM and the record. Deleting the org with
`delete_vm` (`--delete-vm`, a checkbox in the web UI, on by default there)
deletes the VM and forgets the server once the org is gone; without it the
VM keeps running as an empty server, and `isb server rm vm-acme` deletes it
later.

What it costs and what it means:

- The VM's CPUs, memory and disk are its own incus limits on the host. They
  count against nothing else: not the org's quota (which applies inside the
  VM), not another org's, and isb keeps no host-wide budget. Size it so the
  host can hold it.
- The agent port is reachable from the host only: the VM's firewall drops
  everything else, and the agent takes only the control plane's client
  certificate. Other instances on `incusbr0` can reach the VM's address,
  not its agent.
- The org's domains: the VM's agent runs no public listeners, so an org in
  a dedicated VM serves its domains through its own Cloudflare Tunnel
  (`--ingress cloudflare-tunnel`, outbound only, [ingress.md](ingress.md)).
  Its published ports and the load balancer are the VM's own, reached from
  inside it.
- Its isb is the control plane's build, copied at creation; it is not
  upgraded with the control plane.

## Secrets

An org's secrets live on the server that runs its consumers, encrypted to
that agent's own age key (`/var/lib/isb/.config/isb/age.txt` on the box,
made at the agent's first start). `secret_set` and `secret_get` through the
control plane carry the value in transit (TLS both legs) and nothing of it is
written on the control plane. Add a break-glass recipient on each server
(`isb secret reencrypt` there) as on any daemon ([secrets.md](secrets.md)).

## What each side keeps

| Control plane (`<state>/`) | Agent (`/var/lib/isb/`, `/etc/isb-agent/`) |
|---|---|
| `servers/pki/`: the CA and its client certificate | `ca.crt`, `tls.crt`, `tls.key` (0600) |
| `servers/servers.json`: name, address, port, `user@host`, certificate fingerprint and expiry, isb version, firewall sources, and for a dedicated VM its org, project, instance and size | `state/agent/orgs.json`: the orgs placed on it |
| `servers/placement.json`: org to server | everything about those orgs: stacks, apps, deployments, secrets (its own age key), builds, registry, metrics, notifications, jobs, backups, its own audit log |
| `servers/known_hosts`: the boxes' SSH host keys | |
| the identity store (members, invitations, tokens of every org) and the audit log of every call | no users: an internal identity file only for its org bookkeeping |

The control plane stores no secret value, stack definition or workload state
of an org on a server: only where it is.

## Health

Every 10 s the control plane asks each agent for its heartbeat
(`GET /internal/v1/heartbeat`): isb and incus versions, CPU, load, memory,
incus storage, the orgs placed there, the number of stacks, and the agent's
last error event. `isb server ls` and `server_show` show it with the state:
`unknown` (not heard from yet), `up`, `unreachable`.

Three misses in a row (30 s) make a server `unreachable`: an event of kind
`server.unreachable` (level error) on stack `<org>/@servers` for each org on
it and on `system/@servers`, with the server as the service. The first
answer after that is `server.recovered`. Org members see these in their
event feed; notification channels of an org on a server live on that
server, so they hear about it only once it is back.

## Failure modes

| What | Then |
|---|---|
| A server is down or cut off | Its orgs' calls fail with "reach server NAME (connect)" and their workloads are wherever the server left them; other orgs are untouched; `server.unreachable` after 30 s. Merged reads list it under `unreachable`. |
| The control plane is down | Servers keep running their orgs (they reconcile, restart, serve their ingress, run jobs and backups); nobody can change anything, since every caller comes through the control plane. |
| The agent restarts | Its event feed starts over; the control plane notices and follows from the start. |
| The control plane's state directory is lost | The CA goes with it: restore it from backup (it is what every agent trusts). Without a backup, re-run `isb server add` on each box (it reissues the agent's certificate under a new CA) and recreate the placement. |
| A certificate nears expiry | `isb server rotate-cert NAME`; `server_show` has `cert_not_after`. |
| The SSH key leaks | It was used for the bootstrap only: remove it from the box's `authorized_keys`; the control plane never needs it again. |
