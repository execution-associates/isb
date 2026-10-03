---
title: Workspaces and sandboxes
description: An org's long-lived machine, where its people and agents work, and the short-lived sandboxes they make beside it.
order: 5
nav_title: Workspaces
---

A workspace is an org's own machine: a long-lived container with a home that
survives rebuilds, where the org's people and agents keep their sessions,
tools and checkouts. It is also an actor in the org: an org token lives
inside it, so the agents running there administer the org's apps through the
org's MCP and reach nothing outside it. Heavy or risky work goes to
**sandboxes** they make through the same MCP, which expire on their own.

| | Workspace | Sandbox |
|---|---|---|
| What it is | The org's long-lived machine, where its people and agents work | A throwaway instance for a build, a test, an experiment |
| How many | One per org | Any, within the org's quota |
| Lifetime | Months; restarting it ends live sessions | Hours: it expires (24 h) and is deleted when idle (2 h) |
| State | A **home** that survives rebuilding the machine | None worth keeping |
| Identity | It **is** an actor: an org token (full admin by default) lives inside it | `isb.owner` names who made it |

Both live in the org's incus project, on its network, under its quotas and
ACL ([Orgs](orgs.md)). There is no nested incus anywhere: an agent in the
workspace asks the daemon for a sandbox, and the daemon makes it beside the
workspace.

```text
isb workspace create [--image IMAGE] [--name N] [--user dev] [--cpus N] [--memory 8GiB]
                     [--root-size 30GiB] [--home-size 20GiB] [-e KEY=VALUE]...
                     [--secret NAME]... [--token-role viewer|member|admin] [--home-bind DIR]
isb workspace show [NAME] [--json]          status, resources, sessions, token metadata, URL
isb workspace ls [--json]
isb workspace start|stop|restart [NAME] [--yes]
isb workspace rebuild [NAME] [--image IMAGE] --yes
isb workspace update [NAME] [--image I] [--cpus N] [--memory M] [--root-size S]
                     [--home-size S] [--token-role R] [--yes]
isb workspace rm [NAME] [--keep-home] --yes
isb workspace rotate-token [NAME]
isb workspace settings [--max-workspaces N] [--sandbox-expiry 24h] [--sandbox-idle 2h|none]
                       [--home-kind volume|host] [--home-pool POOL]
isb workspace sandboxes [--json]             creator, age, expiry, resources
isb workspace extend SANDBOX [--by 24h] [--idle-timeout 4h|none]
isb workspace ssh-config [--name N] ...      the Host block (isb ssh-config for it)
isb workspace ssh [--name N] ...             the ProxyCommand (isb ssh-proxy for it)
```

All of them take `--org` (or `$ISB_ORG`) and go through `isb serve`. The web
UI's **Workspace** page does the same
([The web UI](../getting-started/web-ui.md#the-workspace)), and
[SSH and herdr](../guides/ssh.md) covers getting a shell in it from your own
terminal.

## The machine

- An unprivileged container in the org's project, named after the workspace
  (`workspace` unless `--name`), from any incus image (`dev-base`,
  `images:ubuntu/24.04`) or the org's own `registry:APP:TAG`. Without
  `--image` it is `dev-base` when the host has that image, else
  `images:ubuntu/24.04`; a local image the host lacks is refused with the
  ones it has. It starts with the host (`boot.autostart`).
- Creating it in an org that does not exist is refused up front (`org X not
  found`), as every `workspace_*` and `sandbox_*` tool is. A create that
  fails (an image, the org's quota) leaves nothing behind: no instance,
  definition or token, and no home it made. A full quota is named with its
  usage and how to raise it ([orgs](orgs.md)).
- **The home** is mounted at the workspace user's home (`--user`, default
  `dev`, made when the image lacks it) and survives rebuilds. It is one of
  two things ([below](#the-home-a-volume-or-a-host-folder)): a managed
  volume, or a host folder the host backs up.
- In an org with a disk quota incus needs a size on every disk, so a
  workspace without `--root-size` gets a 20 GiB root, and a sandbox spec
  without one a 10 GiB root.
- **Rebuild** replaces the machine with a fresh one from its image (or a new
  `--image`, which becomes the workspace's) and mounts the same home again:
  a damaged root is a replaced guest. Anything installed outside the home is
  gone; the token stays.
- **Resizing** (`cpus`, `memory`, root and home size) applies at once.
  `image` changes apply on the next rebuild; `env` and `secrets` (through
  `workspace_update`) are delivered again at once (new login shells see
  them).
- Delete removes the machine and revokes the token. A volume home is deleted
  unless `--keep-home` (a new workspace of the same name then mounts it
  again); a host-folder home is always kept.

## The home: a volume or a host folder

| | Managed volume | Host folder |
|---|---|---|
| Where | `<org>_<name>_home` in a storage pool | `<root>/<org>/home` (`<root>/<org>/<name>/home` for another name) on the host |
| When | the default | `isb serve --workspace-home-root DIR` (`ISB_WORKSPACE_HOME_ROOT`) |
| Size | `--home-size` (20 GiB), grown with `workspace_update`; counts against the org's `--disk` quota | the host's disk; no quota |
| First contents | what the image has at that path (incus `initial.copy`: `dev-base`'s mise and dotfiles), else `/etc/skel` | `/etc/skel` when empty |
| Backups | isb: snapshots, backups to the org's S3 destinations, staged restores ([Volumes](../guides/volumes.md)), on the Home tab | the host's own (restic of the root, say); isb takes none |
| Deleting the workspace | deletes it, unless `--keep-home` | keeps it |

**Volumes: the pool.** A new home volume goes in the org's `home_pool`
setting (platform admins), else `isb serve --workspace-pool POOL`
(`ISB_WORKSPACE_POOL`), else the org's default pool; the workspace keeps the
pool it was created in. The Home tab shows the pool and its driver. The
driver decides the default snapshots: on a copy-on-write pool (zfs, btrfs,
lvm, ceph) a new home is snapshotted `@hourly`, keeping 24; on `dir` (and any
other driver) every snapshot is a **full copy of the home**, as large and as
slow as the home itself, so none are scheduled. Back such a home up to S3
instead, or schedule snapshots by hand with a small keep (two or three); the
Home tab says so.

**Host folders.** With `--workspace-home-root`, a workspace's home is a host
folder the daemon creates (0750, owned by the daemon's user) and binds at
the user's home. The instance maps the daemon's uid 1:1 (`idmap: auto`), and
a workspace user the image lacks is made with that uid, so files have the
same owner inside and on the host; other uids show as `nobody` inside. For
the bind, the org's restricted project is allowed `<root>/<org>`
(`restricted.devices.disk.paths`), recorded on the project as a workspace
home (`user.isb.workspace-homes`) so that `isb org create` and `org_update`
list it whatever bind roots they set; nothing else of the host. An org opts out (or in) with its
`home_kind` setting, `volume` or `host` (platform admins).

Restoring a host-folder home is the host's job and follows the same rule as
isb's staged restores: restore into a folder beside the home (bound or
copied in), compare, copy back what you need; never write over the live home
while the workspace runs.

**Security.** A host-folder home puts host disk into a restricted project by
a path only the operator chooses: `--workspace-home-root` is the daemon's
configuration, `home_kind` is for platform admins, and naming a path per
workspace (`home_bind`) is for superadmins. Org members and the workspace's
own token can set none of them. The folder is readable on the host by its
owner (the daemon's user), and the host's backups hold the org's data, so
they must be kept with the same care as the daemon's state.

### Using an existing folder as the home

To move an existing machine's home into an org's workspace without copying
it, point `--workspace-home-root` at the directory that holds such homes
(say `/srv/workspaces`), stop the old machine, and have a superadmin (the
unix socket, a superadmin token or identity) create the workspace with
`home_bind` naming the folder:

```sh
isb --org acme workspace create --image dev-base --home-bind /srv/workspaces/box/home
```

The folder's name need not match the org's. It is allowed in the org's
project and kept as it is, and its files keep their owner as long as the
workspace user's uid is the daemon's.

## Confirmations and live sessions

Stop, restart, rebuild, delete and resizing end (or can end) every session
on the machine, so the tools refuse without `confirm: true` and say what is
live; the CLI's `--yes` sets it. Agents get the same sentence in the tool's
error:

```
Restarting workspace in org acme ends every session on it (2 web terminal(s) and 1 SSH connection(s)). If that is intended, call again with confirm: true.
```

**Live sessions** are what isb can see: web terminals open through this
daemon, and established SSH connections inside the machine (`ss` on port 22,
asked at most every 30 seconds).

## Who may do what

| | Viewer | Member | Admin, owner, platform admin |
|---|---|---|---|
| See it (`workspace_get`, `workspace_list`, `sandbox_list`) | yes | yes | yes |
| Start, stop, restart; the terminal and SSH (attach) | | yes | yes |
| Create, change, rebuild, delete; rotate the token | | | yes |
| Sandbox defaults (`workspace_settings`) | | | yes |
| `max_workspaces`, `home_kind`, `home_pool` | platform admins only | | |
| `home_bind` (a host path per workspace) | superadmins only | | |

## The workspace is an org actor

Creating a workspace mints its token, `isb_ws_...`, with a role in the org
(`--token-role`, default `admin`: the org's apps, deployments, secrets,
sandboxes and audit log). The token:

- is **confined to its org**: it acts as no user, reaches no other org, no
  platform or host tool, and cannot use the identity endpoints
  (`/api/v1/auth/*`: no members, invitations, tokens or keys);
- arrives inside as **`/run/isb/token`** (0400, owned by the workspace user)
  and, through `/etc/profile.d/isb.sh`, as **`$ISB_TOKEN`** in login shells,
  with **`$ISB_URL`**, **`$ISB_ORG`** and `$ISB_WORKSPACE`. The `isb` CLI
  inside works with no setup (with no daemon socket it uses `$ISB_URL` and
  `$ISB_TOKEN`), and MCP clients use `$ISB_URL/orgs/$ISB_ORG/mcp`
  ([Agents and MCP](../guides/agents.md));
- is **never shown**: no tool returns it, it is never logged or put on a
  command line (it reaches the machine through incus' file API). The daemon
  keeps its SHA-256 for authentication and the token itself encrypted to its
  age key, under `<state>/orgs/<org>/workspaces/` (0600), so it can deliver
  it again after a restart or rebuild (`/run` is empty after a boot; the
  daemon notices a new init process within 15 seconds);
- is **rotated** by `workspace_token_rotate` (the old one fails at once, the
  new one is delivered) and **revoked** by deleting the workspace;
- shows in the audit log and the history as actor **`workspace`** in its
  org, and labels the sandboxes it makes `isb.owner=workspace`.

Named org secrets (`--secret NAME`) are delivered as `/run/isb/secrets/NAME`
(0400, the workspace user's). `-e KEY=VALUE` are plain variables for login
shells (`ISB_*` are isb's and refused).

### Reaching isb from inside: the bridge listener

The workspace reaches isb on its own org's network, never the host's
loopback or a public URL: for each org with a workspace, `isb serve` serves
the org-bound surface on the org bridge's gateway address,
`http://<gateway>:8481` (`--workspace-mcp-port`, `ISB_WORKSPACE_MCP_PORT`).
That is `$ISB_URL`.

- It answers **only peers in the org's own subnet** (others get 403), and
  other orgs' instances cannot route to it anyway: their ACLs reject the
  private ranges.
- It serves **only `/orgs/<org>/...`** (MCP, the REST tools, the `workspace`
  resource, the terminal and SSH websockets) and `/healthz`; the authorizer
  pins every call to the org.
- It takes **bearer tokens only**, the workspace's or an org API token: no
  session cookies, no Access assertions, no superadmin tokens.
- The host firewall must let the bridges reach the port: `sudo isb host
  setup` adds `ufw allow in on isbbr+ to any port 8481 proto tcp`
  ([Host firewall](../operations/host-setup.md#host-firewall)). It must
  differ from the ingress's tunnel port (8480).

For an org placed on a server, the server's agent runs the workspace, keeps
its token and serves the listener on that server's bridge; the control plane
forwards the `workspace_*` tools like any org call.

A `default` org that is incus' own `default` project (no org network) has no
workspace: create one in an org of its own.

## Sandboxes are short-lived

Every sandbox made through `isb serve` (`sandbox_create`) gets deadlines,
kept on the instance as `user.isb.expires_at` and `user.isb.idle_timeout`:

- **an expiry**: the org's `sandbox_expiry` (default 24 h; at most 30 days),
  or the call's `expires`;
- **an idle timeout**: the org's `sandbox_idle` (default 2 h), or the call's
  `idle_timeout`, or `none`. Idle means no exec or terminal through isb and
  under 2 % of a core of CPU since the daemon last saw it used (a daemon that
  just started counts from its start).

`sandbox_extend` pushes the expiry out (`by`, default 24 h, from the later of
now and the current expiry, never past 30 days from now) or changes the idle
timeout; its creator (the `isb.owner` the caller would get) or the org's
admins may. `sandbox_list` shows each instance's kind (`sandbox`,
`workspace`, `replica`, `build`), creator, age, expiry, idle timeout, last
activity, limits and use, and whether the caller made it (`mine`).

**The reaper** in the daemon checks every minute and deletes a sandbox past
either deadline, recording `sandbox.reaped` in the history with the reason.
It only takes instances that carry isb's deadlines and are neither a
workspace, a stack replica nor a build, re-reads each before deleting it,
skips one with a terminal open, and leaves orgs placed on servers to their
agents. Sandboxes made by `isb create` and `isb up` on the host have no
deadlines and are never reaped.

The workspace cannot be reached as a sandbox: `sandbox_create` over its name
and `sandbox_remove` of it are refused.

## The tools

| Tool | Who | Does |
|---|---|---|
| `workspace_get` | members | the workspace (or `null`) and the org's settings: definition, status, resources, home, live sessions, last activity, token metadata, `connect` (`url`, `mcp_url`), sandbox count; with none yet, `create`: the `images` it can be made from, the `default_image`, and the org's `quota` and usage |
| `workspace_list` | members | every workspace in the org (one, unless `max_workspaces` was raised) |
| `workspace_create` | admins | `image` (default above), `name`, `user`, `cpus`, `memory`, `root_size`, `home_size`, `env`, `secrets`, `labels`, `token_role`; `home_bind` (superadmins: a host folder as the home) |
| `workspace_update` | admins | any of those but `name`, `user`, `home_bind`; resizing needs `confirm` |
| `workspace_start`, `workspace_stop`, `workspace_restart` | members | stop and restart need `confirm` |
| `workspace_rebuild` | admins | `image`, `confirm` |
| `workspace_delete` | admins | `keep_home`, `confirm` |
| `workspace_token_rotate` | admins | a new token, delivered; the old one revoked |
| `workspace_settings` | members read, admins change | `sandbox_expiry`, `sandbox_idle`; `max_workspaces`, `home_kind`, `home_pool` (platform admins) |
| `sandbox_create` | members | takes `expires`, `idle_timeout` |
| `sandbox_extend` | the creator, admins | `name`, `by`, `idle_timeout` |

The same as a REST resource, per org:

| Method and path (`/orgs/<org>/api/v1/...`) | Tool |
|---|---|
| `GET workspace[?name=]` | `workspace_get` |
| `POST workspace` | `workspace_create` |
| `PATCH workspace` | `workspace_update` |
| `DELETE workspace` (body `{"confirm": true}`) | `workspace_delete` |
| `POST workspace/start`, `/stop`, `/restart`, `/rebuild`, `/token/rotate` | the action |
| `GET`, `PATCH workspace/settings` | `workspace_settings` |

## Files

Under the org's state directory (`<state>/workspaces/` for the default org,
`<state>/orgs/<org>/workspaces/` for the others), 0600: `<name>.json` (the
definition, the token's id and hash, the home's pool or host folder),
`<name>.token.age` (the token, encrypted to the daemon's key),
`settings.json`. A volume home's snapshot schedule and backups are the
volume's ([Volumes](../guides/volumes.md)).
