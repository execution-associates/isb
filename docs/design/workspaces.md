# Design: workspaces

Status: **accepted, in progress**. This is a plan, not a description of
what isb does today. When a part ships, its user-facing description moves
into the normal docs (`orgs.md`, `web.md`, a new `workspaces.md`) and this
file loses that part.

## The problem: two halves that don't know about each other

isb grew two products in one binary.

- **The platform** (web UI, org MCP, `isb app`): orgs, apps, deployments,
  templates, databases, backups. Disposable, replicated services.
- **Sandboxes** (`isb create`, `isb up`, `isb.yaml`, the TUI): incus
  containers with a shell and whatever a developer or agent installed in
  them.

The web UI is built around the first and shows the second only to
superadmins, as a side list. But the thing `~/projects/titan-iac` exists to
run is a persistent machine per org: the **box**, where that org's agents
(clem for ocai, fitty for 52labs, stub for ticket500, olivia for wistock)
live, hold sessions, and administer the org's apps and nothing else. The org
MCP was built for exactly those agents, yet the platform has no idea they
exist.

## The model

**An org holds apps, one workspace, and the workspace's sandboxes.**

| | App | Workspace | Sandbox |
|---|---|---|---|
| What it is | A service: image or git source, replicas, domains | The org's machine, where its people and agents work | A throwaway instance for a build, a test, an experiment |
| How many | Any | **One per org** | Any, within the org's quota |
| Lifetime | Deployed, rolled out, replaced | Months; restarting it ends live sessions | Hours; reaped when it expires or goes idle |
| State | Named volumes for data | A **home** that survives rebuilding the machine | None worth keeping |
| Reached by | HTTP through ingress | Terminal, SSH (and so herdr), published ports | `sandbox_exec`, or the terminal |
| Identity | None; it is managed | It **is** an actor: its agents hold the org's MCP credentials (full admin by default) | None of its own; it belongs to whoever created it |

All three live in the org's incus project, on its network, under its quotas
and its placement (local, a server, or a dedicated VM). The workspace's
agents administer the org through the org MCP and reach nothing outside it.

**One workspace per org** because "the org's box" should be a single place:
one token, one SSH line, one herdr line, one home to back up. Several agents
and people share it (herdr panes, as in titan-iac today); heavy or risky work
goes to sandboxes so it doesn't starve or endanger the workspace. The cost is
a shared failure domain and no per-person box inside an org; the answer to
"a contractor needs their own machine" is their own org. The API still gives
the workspace a name and an id, so allowing a second one later is a setting,
not a migration.

**Sandboxes are short-lived by rule.** Each has an expiry (default 24 h) and
an idle timeout; its creator can extend it; the daemon reaps it when either
runs out and records that in the history. The workspace page lists them with
who created each, its age, its expiry and its resources. `isb create` and
`isb up` from an `isb.yaml` keep working and make sandboxes (in the org given
by `--org`, or incus' own default project as today), with the same expiry
unless the spec says otherwise.

**Cross-org agents are superadmins.** An agent like Jessica, which works
across every org, runs on the host (or a listed tailnet node) with the
superadmin MCP. Org agents live in their org's workspace with the org MCP.
That is the split titan-iac has today (titan's own shell versus a box), now
stated in one place.

## What maps from titan-iac

| titan-iac | isb |
|---|---|
| Org = k3s namespace + incus project `titan-<org>-ct` + vault + tunnel | Org = incus project `isb-<org>` + network + ACL + secret store + ingress provider |
| Box = unprivileged system container that is also the org's k3s worker | Workspace = unprivileged system container in the org's project; **not** a cluster node |
| `security.nesting=true`, non-ZFS `container-roots` pool, hand-built per-NIC ACL, static address, VXLAN | None of it: those exist only because the box is a kubelet. A workspace is an ordinary instance under the org's rules |
| Apps as k8s workloads, Flux from git | isb apps (image or git source, webhooks, rollouts) |
| Golden image `titan-org-guest` (herdr, ttyd, mise, units) | A **workspace image**, built by isb's build service from a recipe in a repo |
| Home = host directory `/srv/workspaces/<box>/home` bound with `shift=true` | Home = a managed incus volume `<org>_workspace_home`; a bind is allowed during migration (superadmin) |
| `kubectl` + `org-admin` Role + `K8S_TOKEN` through the broker | The org MCP + an isb token minted for the workspace, full admin of the org |
| `secret` CLI → broker → the org's 1Password vault | Org secrets in isb's store (age, pluggable); `isb secret get` with the workspace's token |
| Doors: ssh/ttyd/lasso through cloudflared + Access | Web terminal (exists), `isb workspace ssh` proxy, published ports through ingress |
| `ws-state-snapshot` + hourly restic of `/srv/workspaces` | Volume snapshots on a schedule + backups to the org's S3 destinations |
| `ws:sandbox` nested incus inside a box | Sibling sandboxes through the org MCP; no nesting |
| Changes to a box are attended because they end sessions | Restart, rebuild and resize ask for confirmation and say so; agents get the same warning in the tool result |

## Decisions

- **No nested incus in workspaces.** Agents get sibling sandboxes through
  the org MCP instead: under the org's quota, ACL and audit, visible in the
  UI, with no incus socket inside anything.
- **Docker inside a workspace is an exception**: `security.nesting` stays
  refused, and only a superadmin can allow it, per org, as an org setting.
- **The workspace's token defaults to full admin of its org** (`admin`),
  matching titan-iac's `org-admin`. The create dialog shows it and can
  narrow it.
- **herdr stays outside isb's core.** isb provides the machine, the home,
  SSH, the terminal and the credentials; herdr is what an image chooses to
  run.

## Pieces

W1 (the workspace and its sandboxes), W2 (the workspace as an org actor),
W5 (workspace images) and W7 (the web UI) have shipped:
[workspaces.md](../concepts/workspaces.md),
[workspace-images.md](../guides/workspace-images.md) and
[web-ui.md](../getting-started/web-ui.md#the-workspace).

### W3. Doors

SSH over the daemon's websocket ([ssh.md](../guides/ssh.md)), `isb workspace ssh`
and `ssh-config`, the web terminal on any instance with tabs, terminal
reattach through herdr when the image has it, and the Connect panel's SSH
and herdr lines have shipped. Left:

- **Ports**: the workspace can publish ports through the org's ingress (a dev
  server preview at `<port>.workspace.<domain>`), with the same Access and
  domain rules as apps; the workspace gains a `ports` field and the page a
  Ports tab.

### W4. The home: snapshots, backups, restore

Ships generic over an org's named volumes ([volumes.md](../volumes.md)):
scheduled and manual snapshots with retention, volume backups to the org's
S3 destinations beside the databases', the `/etc/isb/pre-snapshot` hook,
and staged restores into a new volume at `/restore/<stamp>` (titan-iac's
`ws-rollback.sh` rule: never a second writer over a live home). The Volume
panel (`web/src/volumes/volume-panel.tsx`) is self-contained. The workspace's Home tab
embeds the panel for a volume home, with a default schedule by pool driver
([workspaces.md](../workspaces.md#the-home-a-volume-or-a-host-folder)).
Left: rebuild and delete say what happens to a home's staged restores.

### W5. Workspace images

Recipe-script images, isb's default recipe (`isb-workspace`) and first-boot
scripts have shipped. Left: a recipe kept in a repository and rebuilt on
push, and workspace templates (a recipe plus a first-boot script) in the
template catalog.

### W6. The Docker exception

An org setting `allow_nesting` (superadmin only, audited) lets the
workspace, and only the workspace, run with `security.nesting=true` for
Docker. Sandboxes never get it. The org page shows it as a warning badge.

## Migrating a titan-iac org

Per org, attended, one at a time:

1. `isb org create <org>` (local, or `--vm` for a tenant that should get its
   own kernel).
2. Secrets: copy the org's vault items the apps and box need into the org's
   isb secrets (by name; values never pass through a chat or log).
3. Workspace: create it from the workspace image; import the home with a
   one-time copy from `/srv/workspaces/<box>/home` into the home volume with
   the box stopped (or bind it during a trial period).
4. Apps: port each k8s workload to an isb app (image, env, volumes,
   domains); databases with `isb db` and a restore from the k8s backup.
5. Doors: route the hostnames through the org's ingress (Cloudflare
   provider); retire the k3s door Services.
6. Point the org's agent at its new home: the workspace's token replaces
   `K8S_TOKEN`; the org MCP replaces `kubectl`.
7. Decommission the box, the namespace and the k3s node once the org has run
   on isb for an agreed period.

## Not doing

- Making herdr a dependency of isb.
- Nested incus in workspaces.
- Workspaces shared between orgs. Collaboration across orgs happens on the
  host, as superadmin, the way it does today.
- Kubernetes compatibility.

## Decided while building

- **The home counts against the org's `--disk` quota**: it is the org's data.
- **Who may attach**: every org member; viewers may not (the terminal and SSH
  are admitted as `sandbox_exec`).
- **Sandbox defaults**: 24 h expiry and 2 h idle, per org
  (`workspace_settings`).
- **The home is a volume or a host folder.** A managed volume by default
  (its pool configurable per host and per org; hourly snapshots only on a
  copy-on-write pool, since on `dir` each is a full copy). Or, with
  `--workspace-home-root`, a host folder `<root>/<org>/home` that the host
  backs up (restic), as titan-iac's `/srv/workspaces` homes are: isb's
  snapshots and S3 backups give way to the host's, in exchange for no copy
  cost and one backup for every org. Choosing host disk for an org is the
  operator's (the flag), platform admins' (`home_kind`) and, per workspace,
  superadmins' (`home_bind`, for a box whose name is not the org's); never
  an org member's or the workspace's own.

- **Plain `isb create` and `isb up` sandboxes get no deadlines**: only
  sandboxes made through `isb serve` expire, so the reaper never takes a
  developer's long-running `isb up` on the host.

## Order of work

W6, then W3's ports. Each lands with its docs,
tests and a live check on titan, like the platform phases did.
