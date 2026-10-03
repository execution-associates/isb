# Design: workspaces

Status: **proposed**. This is a plan, not a description of what isb does
today. When a part ships, its user-facing description moves into the normal
docs (`orgs.md`, `web.md`, a new `workspaces.md`) and this file loses that
part.

## The problem: two halves that don't know about each other

isb grew two products in one binary.

- **The platform** (web UI, org MCP, `isb app`): orgs, apps, deployments,
  templates, databases, backups. Disposable, replicated services.
- **Sandboxes** (`isb create`, `isb up`, `isb.yaml`, the TUI): long-lived
  incus containers with a shell, a home and whatever a developer or agent
  installed in them.

The web UI is built around the first and shows the second only to
superadmins, as a side list. But the thing `~/projects/titan-iac` exists to
run is the second: every org has a **box**, a persistent machine where that
org's agents (clem for ocai, fitty for 52labs, stub for ticket500, olivia for
wistock) live, hold sessions, and administer the org's apps and nothing else.
The org MCP was built for exactly those agents, yet the platform has no idea
they exist.

## The model

**An org holds two kinds of things: apps and workspaces.**

| | App | Workspace |
|---|---|---|
| What it is | A service: image or git source, replicas, domains | A machine where people and agents work |
| Lifecycle | Deployed, rolled out, replaced; disposable | Created once, lives for months; restarting it ends live sessions |
| State | Named volumes for data, nothing else | A **home** that survives rebuilding the machine |
| Reached by | HTTP through ingress | Terminal, SSH (and so herdr), its own ports for previews |
| Identity | None; it is the thing being managed | It **is** an actor: its agents hold the org's MCP credentials |

Both live in the org's incus project, on its network, under its quotas and
its placement (local, a server, or a dedicated VM). A workspace's agents
administer the org through the org MCP; they reach nothing outside the org.

**Sandboxes become workspaces.** There is one concept, with a lifecycle flag:

- `persistent` (the default from the web UI and MCP): the home is a managed
  volume that outlives the instance; snapshots and backups apply; deleting
  the workspace asks about the home separately.
- `ephemeral` (the default for `isb create` and `isb up` from an
  `isb.yaml`): today's sandbox behaviour, gone on `isb down`.

The CLI keeps its verbs. The UI, the MCP and the docs say "workspace".
`sandbox_*` tools stay as aliases for a release, then go.

**Cross-org agents are superadmins.** An agent like Jessica, which works
across every org, runs on the host (or a listed tailnet node) with the
superadmin MCP. Org agents live in their org's workspace with the org MCP.
That is the same split titan-iac has today (titan's own shell versus a box),
now stated in one place.

## What maps from titan-iac

| titan-iac | isb |
|---|---|
| Org = k3s namespace + incus project `titan-<org>-ct` + vault + tunnel | Org = incus project `isb-<org>` + network + ACL + secret store + ingress provider |
| Box = unprivileged system container that is also the org's k3s worker | Workspace = unprivileged system container in the org's project; **not** a cluster node |
| `security.nesting=true`, non-ZFS `container-roots` pool, hand-built per-NIC ACL, static address, VXLAN | None of it: those exist only because the box is a kubelet. A workspace is an ordinary instance under the org's rules |
| Apps as k8s workloads, Flux from git | isb apps (image or git source, webhooks, rollouts) |
| Golden image `titan-org-guest` (herdr, ttyd, mise, units) | A **workspace image**, built by isb's build service from a recipe in a repo |
| Home = host directory `/srv/workspaces/<box>/home` bound with `shift=true` | Home = a managed incus volume `<ws>_home`; a bind is allowed for migration (superadmin) |
| `kubectl` + `org-admin` Role + `K8S_TOKEN` through the broker | The org MCP + an isb token minted for the workspace |
| `secret` CLI → broker → the org's 1Password vault | Org secrets in isb's store (age, pluggable); `isb secret get` with the workspace's token |
| Doors: ssh/ttyd/lasso through cloudflared + Access | Web terminal (exists), `isb workspace ssh` proxy, published ports through ingress |
| `ws-state-snapshot` + hourly restic of `/srv/workspaces` | Volume snapshots on a schedule + backups to the org's S3 destinations |
| `ws:sandbox` nested incus inside a box | Sibling instances through the org MCP (`sandbox_create` in the org); nesting only by exception |
| Changes to a box are attended because they end sessions | Restart, rebuild and resize ask for confirmation and say so; agents get the same warning in the tool result |

## Pieces

### W1. The workspace object

- `workspace_create/list/get/update/start/stop/restart/rebuild/delete` tools,
  `/api/v1/workspaces` REST, `isb workspace ...` CLI. Org admins and above
  create and delete; members start, stop and attach.
- Fields: name, image, cpus, memory, root size, home size, `persistent` or
  `ephemeral`, environment, secrets by name, ports to publish, labels.
- **Rebuild** replaces the instance from the image and reattaches the home:
  the titan-iac "a damaged root is a replaced guest" rule as one button.
- Built on the existing sandbox machinery (it already creates instances in an
  org project); `isb.owner` and history events as today.
- Status reports live sessions where it can tell (attached terminals, SSH
  connections), so "restart" can say what it will end.

### W2. Workspaces are org actors

- Creating a workspace mints an org token bound to it (`token:ws-<name>`):
  role `admin` in its org by default, narrower by choice (`deploy`, `read`,
  tool globs). Revoked when the workspace is deleted; rotatable from the UI.
- It arrives inside as a file (`/run/isb/token`, 0400, owned by the
  workspace user) and as `ISB_TOKEN` in login shells, with `ISB_URL` and
  `ISB_ORG`. The `isb` CLI inside works with no setup; `claude mcp add`
  and the MCP page's snippets work as written.
- **Reaching the control plane from inside the org:** a per-org MCP listener
  on the org's bridge address, beside the ingress's `:8480`, so a workspace
  never needs the host's loopback or a public URL. For an org on a server,
  the server's agent serves it and forwards like any org call.
- Audit and history show `workspace:<name>` as the actor.

### W3. Doors

- **Web terminal**: already exists for apps; extend to workspaces, with
  several tabs and reattach.
- **SSH without opening ports**: `isb workspace ssh <ws>` speaks SSH's stdio
  over the daemon's authenticated websocket (`ProxyCommand isb workspace
  proxy %h`). `isb workspace ssh-config` prints `Host` blocks, so plain `ssh`,
  `scp`, editors and `herdr machine add` all work. Keys: the user's public
  keys from their isb account, installed for the workspace user.
- **Ports**: a workspace can publish ports through the org's ingress (a dev
  server preview at `ws-<name>-<port>.<domain>`), with the same Access and
  domain rules as apps.
- **herdr stays outside isb's core.** isb provides the box, the home, SSH,
  the terminal and the credentials; herdr is what an image chooses to run.
  The UI shows a ready `herdr machine add` line next to the SSH config.

### W4. The home: snapshots, backups, restore

- Scheduled snapshots of `<ws>_home` (incus volume snapshots), kept N.
- Backups to the org's existing S3 destinations, on the backups page beside
  databases.
- **Restore is staged, never in place**: a snapshot or backup restores into a
  new volume mounted at `/restore/<stamp>` inside the workspace, for its
  owner to diff and copy. That is titan-iac's `ws-rollback.sh` rule, because
  a second writer over a live home is how work is lost.
- Application-consistent copies of agent state (SQLite under `~/.claude`,
  herdr's `session.json`) are a hook the image provides
  (`/etc/isb/pre-snapshot`), not something isb knows about.

### W5. Workspace images

- A workspace image is built like an app image (build service, registry)
  from a recipe repo, or is any incus image (`dev-base`, `images:ubuntu/...`).
- Templates gain a workspace kind: "Claude Code + herdr + mise",
  "plain Ubuntu", and the user's own. The titan-iac `titan-org-guest` recipe
  becomes one.
- Software that must survive a rebuild lives outside the home (`/opt/...`),
  the titan-iac lesson about `~/.local/bin` shadowing the image.

### W6. Sandboxes from inside a workspace

Agents in a titan-iac box get nested incus (`ws:sandbox`). In isb, an agent
in a workspace asks the org MCP for a **sibling** sandbox instead:
`sandbox_create` in the same org, ephemeral, under the org's quota, ACL and
audit. No nesting, no incus socket inside, and the sibling is visible in the
UI. Nesting (`security.nesting` for Docker-in-workspace) stays refused by
default and is an org setting only a superadmin can turn on, per org.

### W7. The web UI

- **Org overview**: Workspaces first (status, attached sessions, CPU and
  memory, last activity), then Apps.
- **Sidebar**: Workspaces next to Projects.
- **Workspace page**: Terminal (tabs), Connect (SSH config, herdr line, MCP
  credential with rotate and revoke), Resources, Home (size, snapshots,
  backups, staged restore), Environment and secrets, Ports, History.
- **Create dialog**: image or workspace template, size, persistent or
  ephemeral, the token's role, placement shown read-only from the org.
- The superadmin Host page keeps showing every instance on the host,
  including ones no org owns.

## Migrating a titan-iac org

Per org, attended, one at a time:

1. `isb org create <org>` (local, or `--vm` for a tenant that should get its
   own kernel).
2. Secrets: copy the org's vault items the apps and box need into the org's
   isb secrets (by name; values never pass through a chat or log).
3. Workspace: create it from the workspace image; import the home with a
   one-time copy from `/srv/workspaces/<box>/home` into `<ws>_home` with the
   box stopped (or bind it during a trial period).
4. Apps: port each k8s workload to an isb app (image, env, volumes,
   domains); databases with `isb db` and a restore from the k8s backup.
5. Doors: route the hostnames through the org's ingress (Cloudflare
   provider); retire the k3s door Services.
6. Point the org's agent at its new home: the workspace's own token replaces
   `K8S_TOKEN`; the org MCP replaces `kubectl`.
7. Decommission the box, the namespace and the k3s node once the org has run
   on isb for an agreed period.

## Not doing

- Making herdr a dependency of isb.
- Workspaces shared between orgs. Collaboration across orgs happens on the
  host, as superadmin, the way it does today.
- Kubernetes compatibility.

## Open questions

1. **Home size and quota**: does a workspace's home count against the org's
   `--disk` quota? (Proposed: yes; it is the org's data.)
2. **Who may attach**: every org member, or a per-workspace member list?
   (Proposed: org members; a per-workspace list later if needed.)
3. **Idle policy**: should ephemeral workspaces stop after N hours idle?
4. **Token role default**: `admin` matches titan-iac's `org-admin`; `deploy`
   is safer. (Proposed: `admin`, shown in the create dialog.)
5. **Home bind for migration**: allow binding a host directory as the home
   permanently for superadmins, or only during migration?

## Order of work

W1 and W7's workspace list first (the split brain goes away in the UI), then
W2 (the reason workspaces exist), W3, W4, W6, W5. Each lands with its docs,
tests and a live check on titan, like the platform phases did.
