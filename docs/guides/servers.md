---
title: Servers and dedicated VMs
description: Run whole orgs on other hosts, or in a VM of their own, all managed from one control plane.
order: 16
---

One `isb serve` can be the **control plane** for others. Each other host is a
**server**: it runs incus and `isb serve --agent`, and the control plane places
whole orgs on it. Users, agents and the web UI only ever talk to the control
plane; it forwards each call for an org to the server that holds the org.
Use it to spread orgs over machines, to give a customer a host of their own,
or, with a **dedicated VM**, to give an org its own kernel on the same host.
Why you would pick each is in [Placement](../concepts/placement.md).

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
   |  orgs placed "local" run here                  |
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
   signed `SHA256SUMS`. The box checks the upload's SHA-256 again before installing it
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
   --state-dir /var/lib/isb/state`, and starts it, with the upgrade helper
   ([Upgrading servers](#upgrading-servers));
6. waits for the agent's heartbeat over mTLS and checks it presents the
   certificate just issued.

Every step is idempotent: rerunning `isb server add` after a failure
converges. The CLI prints each step and the box's output as they happen:
it calls `server_add` with `wait: false`, which answers at once, and
follows `server_provision_get` (steps with their state and times, the log,
the error). The web UI's **Add server** wizard (Platform, Servers) does the
same, and offers Retry on a failure. A run's progress stays readable for an
hour after it fails (and ten minutes after it succeeds), in memory only; one
run per server name at a time. Without `wait: false`, `server_add` answers
when the server is up.

The SSH key is used for this and never again; the control plane
keeps only `user@host`, the box's host key (`servers/known_hosts`) and what
it dials. SSH runs with exactly that key (`-F /dev/null`, `IdentitiesOnly`,
no agent). Through the API, `key` (a path) is for the local CLI only; a remote
platform admin sends `ssh_key`, the key itself, which is written 0600 for the
bootstrap and deleted after. The web wizard sends it once and clears it from
the form.

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

`server_list` also suggests the addresses this control plane's traffic leaves
from (`suggested_allow_from`), which the wizard prefills.

`isb server rm NAME` forgets a server; it is refused while orgs are placed on
it. The agent keeps running on the box until it is stopped there
(`systemctl disable --now isb-agent`); a [dedicated VM](#dedicated-vms) is
deleted with its server. `isb server rotate-cert NAME` (or **Rotate certificate** on the server in
the web UI's Admin, Servers) issues the
agent a new certificate and key over the current mTLS connection; the agent
writes them and uses them for every new connection, and the control plane
checks that it does.

## Placing an org

```sh
isb org create acme --server hel-1 [--cpus 4] [--memory 8GiB] [--allow-egress ...]
isb org create beta --vm [--vm-cpus 4 --vm-memory 8GiB --vm-disk 100GiB]
```

(`org_create` with `placement: {"server": "hel-1"}`, or `server: "hel-1"`;
platform admins.) The control plane tells the agent the org is placed on it,
creates it there (its incus project, bridge and ACL), and adds it to its own
identity store for members, invitations and tokens. The default org is always
local. Bind roots of an org on a server cannot be set through the control
plane; its domain allowlist and ingress provider are `org_create`'s and
`org_update`'s, passed on to its server. An org does not move once
placed ([Moving an org](../concepts/placement.md#moving-an-org)).

From then on every org-scoped call for that org goes to its server, on every
surface: MCP (`/mcp` and `/orgs/<org>/mcp`), REST, the CLI over the control
plane's socket, the web terminal's and SSH websockets (bridged
through to the agent's; [SSH to an org on a server](#ssh-to-an-org-on-a-server))
and app webhooks (forwarded as they came; the agent checks the signature,
since it holds the app's secret).
The control plane authenticates the caller, runs its authorizer (roles, token
scopes, org scope) and writes its audit row first; the agent then judges the
asserted caller again, in the org the call was sent for:

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

For an org on a server, the server's agent also runs the org's
[workspace](../concepts/workspaces.md), keeps its token and serves the bridge
listener on that server's bridge (port 8481; `isb host setup`, run by the
bootstrap, opens it to the org bridges in the box's firewall); the control
plane forwards the `workspace_*` tools like any org call. The bridge
listener there takes the workspace's own token only: org API tokens live in
the control plane's identity store, so a workspace reaches isb through its
token, and people and agents outside reach it through the control plane.
The agent reaps the org's expired and idle sandboxes; the control plane's
reaper leaves them alone.

Volume [snapshots, backups and staged restores](volumes.md) run on the agent
too, with the org's backup destinations and their keys (org secrets) kept
there. The destination must be reachable from the server: an S3 store on the
control plane's loopback is not.

## SSH to an org on a server

`isb ssh-proxy` (and so `ssh`, `scp`, editors and herdr) works for instances
of an org on a server exactly as for a local one ([SSH](ssh.md)). The
control plane admits the websocket, writes `ssh.open`, reads the caller's
SSH public keys from its identity store at that moment, and opens the same
websocket on the agent with the caller asserted and the keys in a header
(`X-Isb-Ssh-Keys`, base64url JSON). The agent checks the org as for any
forwarded call, parses each key again (options refused), writes them for
`sshd -i` in the instance as it does locally, and tells the control plane
which key sshd accepted (a text frame the client never sees).

The grant is checked on the control plane, where the account lives: every
15 seconds, as for a local session (key removed, account disabled, token
revoked or expired, sign-in ended, membership dropped or demoted to
viewer). A failed check ends the bridge with the reason, and closing the
bridge ends the agent's sshd. Both sides keep audit rows: the control
plane's `ssh.open` and `ssh.close` (guest user, key fingerprint, duration),
and the agent's own. An agent older than protocol 2 (below) refuses SSH:
"server NAME runs isb ... (agent protocol 1); this needs protocol 2:
upgrade it with `isb server upgrade NAME`".

## Upgrading servers

```sh
isb server upgrade hel-1                     # to this control plane's own build
isb server upgrade --all
isb server upgrade hel-1 --isb-version 1.0.1 # a release, checked against its signed SHA256SUMS
isb server upgrade hel-1 --isb-binary ./isb  # a Linux build on this host
```

`server_upgrade` (`name` or `all: true`, `version`, `isb_binary` for the
local CLI only; platform admins) replaces a server's agent and waits until it
answers with the new build:

1. It reads the agent's heartbeat (it must answer) for its architecture and
   the build it runs (`build`: the SHA-256 of its binary), and picks the
   binary: by default the control plane's own executable, checked to be a
   Linux build for that architecture. The same build already running is
   left alone.
2. It hands the binary over. **A server added over SSH** gets it over the
   agent's mTLS connection, in 3 MiB chunks (`/internal/v1/upgrade/chunk`),
   then `/internal/v1/upgrade/apply` with its SHA-256: the agent checks size,
   hash and architecture and stages it as `<state>/upgrade/isb.new`. **A
   dedicated VM** gets it through the incus API (file push, then exec), as
   at creation, which also installs the helper below on a VM made before it
   existed.
3. On the box, the root helper (`isb-agent-upgrade.path` watches for the
   staged request and starts `isb-agent-upgrade.service`, which runs
   `/usr/local/lib/isb/agent-upgrade`) copies the binary out of the agent's
   directory, checks the SHA-256 again on its copy, runs `isb --version` with
   it as the `isb` user, keeps the old binary as `/usr/local/bin/isb.prev`,
   replaces `/usr/local/bin/isb` atomically and restarts `isb-agent`.
4. The control plane waits (up to 100 s) for the heartbeat with the new
   build, then confirms it (`/internal/v1/upgrade/confirm`). Without that
   confirmation within 120 s of the restart, the helper puts `isb.prev` back
   and restarts the agent again; the control plane waits to see that and says
   so ("... the server restored its previous binary and answers again").

Each takes seconds plus the agent's restart; calls for the server's orgs
fail meanwhile ("reach server NAME") and its workloads keep running. The
helper's last run is in the heartbeat (`upgrade.last`: `restarting`,
`done`, `failed`, `rolled_back`) and on the Servers page.

Why this way: the bootstrap's SSH key is used once and never kept, and
keeping one would make the control plane hold a standing root credential for
every box. The mTLS connection already carries every call the control plane
makes; only the control plane's client certificate opens it, and the upgrade
routes take only the control plane's own assertion. The agent's user is in
`incus-admin`, which is root-equivalent on the box, so a control plane that
can drive the agent could already act as root there; replacing the binary
gives it nothing new. What the upgrade adds is integrity and a way back: the
hash is checked by the agent and again by root on its own copy, the binary
must run before it is installed, and the box (not the control plane, which
cannot reach an agent that did not come back) restores the previous binary.

### Version skew

`server_list` and `server_show` answer `version`: the agent's `isb`,
`build` and `protocol`, the control plane's, `skew` (another version or
build), `compatible`, `ssh` (SSH forwarding needs protocol 2),
`upgradable` (a dedicated VM, or a server with the helper) and
`last_upgrade`. `isb server ls` marks a different build `(differs)`; the
Servers page shows a badge and, in the server's details, its build next to
the control plane's with an **Upgrade** button.

The agent protocol is how the two sides talk (1: the first; 2: SSH
forwarding, the upgrade routes, and `protocol`, `build` and `arch` in the
heartbeat). The control plane forwards calls to agents speaking protocol 1
or 2 and refuses a newer one with "server NAME runs isb ... (agent protocol
N), newer than this control plane understands (protocol 2): upgrade the
control plane". A server added before the helper existed (by an isb without
`server_upgrade`) has no upgrade routes: replace its binary by hand once
([Upgrading isb](../operations/upgrades.md#servers-and-dedicated-vms)).

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

`isb serve --agent` is that agent: no identity store, web UI or `--listen`,
only the mTLS listener (`--agent-listen`, any address: the client
certificate is the gate) with its TLS directory (`--agent-tls`: `ca.crt`,
`tls.crt`, `tls.key`).

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
`server_provision_get` with name `vm-acme`, as the CLI and the web UI's New
org dialog do. The org's own settings (quota, egress) go with it as for any
placement and are checked before the VM is made. It takes minutes.

A dedicated VM is a server the control plane provisioned itself, through
incus rather than SSH:

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
  (`--ingress cloudflare-tunnel`, outbound only; [Domains](domains.md#cloudflare-tunnel-provider)).
  Its published ports and the load balancer are the VM's own, reached from
  inside it.
- Its isb is the control plane's build, copied at creation;
  `isb server upgrade vm-ORG` brings it to the control plane's current build
  ([Upgrading servers](#upgrading-servers)).

## Secrets

An org's secrets live on the server that runs its consumers, encrypted to
that agent's own age key (`/var/lib/isb/.config/isb/age.txt` on the box,
made at the agent's first start). `secret_set` and `secret_get` through the
control plane carry the value in transit (TLS both legs) and nothing of it is
written on the control plane. Add a break-glass recipient on each server
(`isb secret reencrypt` there) as on any daemon ([Secrets](secrets.md#break-glass-recipients)).

## What each side keeps

| Control plane (`<state>/`) | Agent (`/var/lib/isb/`, `/etc/isb-agent/`) |
|---|---|
| `servers/pki/`: the CA and its client certificate | `ca.crt`, `tls.crt`, `tls.key` (0600) |
| `servers/servers.json`: name, address, port, `user@host`, certificate fingerprint and expiry, isb version, firewall sources, and for a dedicated VM its org, project, instance and size | `state/agent/orgs.json`: the orgs placed on it |
| | `state/upgrade/`: an upgrade being staged and the helper's last result; `/usr/local/bin/isb.prev`, the binary before the last upgrade; the helper (`/usr/local/lib/isb/agent-upgrade`, `isb-agent-upgrade.path` and `.service`) |
| `servers/placement.json`: org to server | everything about those orgs: stacks, apps, deployments, secrets (its own age key), builds, registry, metrics, notifications, jobs, backups, its own audit log |
| `servers/known_hosts`: the boxes' SSH host keys | |
| the identity store (members, invitations, tokens of every org) and the audit log of every call | no users: an internal identity file only for its org bookkeeping |

The control plane stores no secret value, stack definition or workload state
of an org on a server: only where it is. Back up its `servers/` directory
with the rest of its state: the CA is what every agent trusts.

## Health

Every 10 s the control plane asks each agent for its heartbeat
(`GET /internal/v1/heartbeat`): isb and incus versions, the agent's build,
architecture and protocol, its upgrade helper, CPU, load, memory,
incus storage, the orgs placed there, the number of stacks, and the agent's
last error event. `isb server ls` and `server_show` show it with the state:
`unknown` (not heard from yet), `up`, `unreachable`. The web UI's Servers
page shows the same, with how long since the last heartbeat.

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
| A certificate nears expiry | `isb server rotate-cert NAME`, or **Rotate certificate** in Admin, Servers; `server_show` has `cert_not_after`. |
| An upgraded agent does not come back | The helper on the box restores the previous binary 120 s after the restart; `server_upgrade` fails saying so, and `upgrade.last` is `rolled_back`. |
| An agent speaks another protocol | Calls for its orgs are refused with the reason: upgrade the older side. |
| The SSH key leaks | It was used for the bootstrap only: remove it from the box's `authorized_keys`; the control plane never needs it again. |

## Tools

| Tool | Does |
|---|---|
| `server_add` | Bootstrap a server over SSH (`wait: false` to answer at once). |
| `server_list` | Servers with their health and orgs, servers being added, whether this host can run dedicated VMs, and suggested firewall sources. |
| `server_show` | One server. |
| `server_remove` | Forget one (refused while it holds orgs; a dedicated VM is deleted with it). |
| `server_rotate_cert` | Issue its agent a new certificate. |
| `server_upgrade` | Replace a server's agent (or every server's) with this control plane's build or a release, and wait for it ([Upgrading servers](#upgrading-servers)). |
| `server_provision_get` | Follow a server (or dedicated VM) being added: steps, log, error. |

All are for platform admins.
