# Workspaces: the org's machine

An org holds apps, one **workspace**, and the workspace's **sandboxes**.

| | Workspace | Sandbox |
|---|---|---|
| What it is | The org's long-lived machine, where its people and agents work | A throwaway instance for a build, a test, an experiment |
| How many | One per org | Any, within the org's quota |
| Lifetime | Months; restarting it ends live sessions | Hours: it expires (24 h) and is deleted when idle (2 h) |
| State | A **home** volume that survives rebuilding the machine | None worth keeping |
| Identity | It **is** an actor: an org token (full admin by default) lives inside it | `isb.owner` names who made it |

Both live in the org's incus project, on its network, under its quotas and
ACL. The workspace's agents administer the org through the org MCP and
reach nothing outside it; heavy or risky work goes to sandboxes they make
through the same MCP (no nested incus anywhere).

```text
isb workspace create --image IMAGE [--name N] [--user dev] [--cpus N] [--memory 8GiB]
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
isb workspace sandboxes [--json]             creator, age, expiry, resources
isb workspace extend SANDBOX [--by 24h] [--idle-timeout 4h|none]
isb workspace ssh-config [--name N] ...      the Host block (isb ssh-config for it)
isb workspace ssh [--name N] ...             the ProxyCommand (isb ssh-proxy for it)
```

All of them take `--org` (or `$ISB_ORG`) and go through `isb serve`
([serve.md](serve.md)). The web UI's **Workspace** page does the same
([web.md](web.md#the-workspace)).

## The machine

- An unprivileged container in the org's project, named after the
  workspace (`workspace` unless `--name`), from any incus image (`dev-base`,
  `images:ubuntu/24.04`) or the org's own `registry:APP:TAG`. It starts with
  the host (`boot.autostart`).
- **The home** is a managed volume `<org>_<name>_home` (`--home-size`,
  default 20 GiB) mounted at the workspace user's home (`--user`, default
  `dev`; made when the image lacks it, uid as `useradd` picks). A new home
  is seeded from what the image has at that path (incus `initial.copy`;
  `dev-base`'s mise and dotfiles), else from `/etc/skel`, and is owned by
  the user.
- **The home counts against the org's `--disk` quota**: it is the org's data.
  In an org with a disk quota incus needs a size on every disk, so a
  workspace without `--root-size` gets a 20 GiB root, and a sandbox spec
  without one a 10 GiB root.
- **Rebuild** replaces the machine with a fresh one from its image (or a
  new `--image`, which becomes the workspace's) and mounts the same home
  again: a damaged root is a replaced guest. Anything installed outside the
  home is gone; the token stays.
- **Resizing** (`cpus`, `memory`, root and home size) applies at once.
  `image` changes apply on the next rebuild; `env` and `secrets` are
  delivered again at once (new login shells see them).
- Delete removes the machine, revokes the token, and deletes the home unless
  `--keep-home` (a new workspace of the same name then mounts it again).

**Confirmation.** Stop, restart, rebuild, delete and resizing end (or can
end) every session on the machine, so the tools refuse without
`confirm: true` and say what is live; the CLI's `--yes` sets it. Agents get
the same sentence in the tool's error:

```
Restarting workspace in org acme ends every session on it (2 web terminal(s) and 1 SSH connection(s)). If that is intended, call again with confirm: true.
```

**Live sessions** are what isb can see: web terminals open through this
daemon, and established SSH connections inside the machine (`ss` on port
22, asked at most every 30 seconds).

## Who may do what

| | Viewer | Member | Admin, owner, platform admin |
|---|---|---|---|
| See it (`workspace_get`, `workspace_list`, `sandbox_list`) | yes | yes | yes |
| Start, stop, restart; the terminal and SSH (attach) | | yes | yes |
| Create, change, rebuild, delete; rotate the token | | | yes |
| Sandbox defaults (`workspace_settings`) | | | yes |
| `max_workspaces`; `home_bind` | platform admins / superadmins only | | |

## The workspace is an org actor

Creating a workspace mints its token, `isb_ws_...`, with a role in the org
(`--token-role`, default `admin`: the org's apps, deployments, secrets,
sandboxes and audit log). The token:

- is **confined to its org**: it acts as no user, reaches no other org, no
  platform or host tool, and cannot use the identity endpoints
  (`/api/v1/auth/*`: no members, invitations, tokens or keys);
- arrives inside as **`/run/isb/token`** (0400, owned by the workspace
  user) and, through `/etc/profile.d/isb.sh`, as **`$ISB_TOKEN`** in login
  shells, with **`$ISB_URL`**, **`$ISB_ORG`** and `$ISB_WORKSPACE`. The
  `isb` CLI inside works with no setup (with no daemon socket it uses
  `$ISB_URL` and `$ISB_TOKEN`), and the MCP page's snippets work as written
  with `$ISB_URL/orgs/$ISB_ORG/mcp`;
- is **never shown**: no tool returns it, it is never logged or put on a
  command line (it reaches the machine through incus' file API). The daemon
  keeps its SHA-256 for authentication and the token itself encrypted to
  its age key, under `<state>/orgs/<org>/workspaces/` (0600), so it can
  deliver it again after a restart or rebuild (`/run` is empty after a
  boot; the daemon notices a new init process within 15 seconds);
- is **rotated** by `workspace_token_rotate` (the old one fails at once, the
  new one is delivered) and **revoked** by deleting the workspace;
- shows in the audit log and the history as actor **`workspace`** in its
  org, and labels the sandboxes it makes `isb.owner=workspace`.

Named org secrets (`--secret NAME`) are delivered as
`/run/isb/secrets/NAME` (0400, the workspace user's). `-e KEY=VALUE` are
plain variables for login shells (`ISB_*` are isb's).

### Reaching isb from inside: the bridge listener

The workspace reaches isb on its own org's network, never the host's
loopback or a public URL: for each org with a workspace, `isb serve`
serves the org-bound surface on the org bridge's gateway address,
`http://<gateway>:8481` (`--workspace-mcp-port`, `ISB_WORKSPACE_MCP_PORT`).
That is `$ISB_URL`.

- It answers **only peers in the org's own subnet** (others get 403), and
  other orgs' instances cannot route to it anyway: their ACLs reject the
  private ranges.
- It serves **only `/orgs/<org>/...`** (MCP, the REST tools, the
  `workspace` resource, the terminal and SSH websockets) and `/healthz`;
  the authorizer pins every call to the org.
- It takes **bearer tokens only**, the workspace's or an org API token:
  no session cookies, no Access assertions, no superadmin tokens.
- The host firewall must let the bridges reach the port: `sudo isb host
  setup` adds `ufw allow in on isbbr+ to any port 8481 proto tcp`
  ([orgs.md](orgs.md#host-firewall-isb-host-setup)). It must differ from
  the ingress's tunnel port (8480).

For an org placed on a server, the server's agent runs the workspace, keeps
its token and serves the listener on that server's bridge; the control plane
forwards the `workspace_*` tools like any org call.

The default org on a host where it is incus' own `default` project (no org
network) has no workspace: create one in an org of its own.

## Sandboxes are short-lived

Every sandbox made through `isb serve` (`sandbox_create`) gets deadlines,
kept on the instance as `user.isb.expires_at` and `user.isb.idle_timeout`:

- **an expiry**: the org's `sandbox_expiry` (default 24 h; at most 30
  days), or the call's `expires`;
- **an idle timeout**: the org's `sandbox_idle` (default 2 h), or the call's
  `idle_timeout`, or `none`. Idle means no exec or terminal through isb and
  under 2 % of a core of CPU since the daemon last saw it used (a daemon
  that just started counts from its start).

`sandbox_extend` pushes the expiry out (`by`, default 24 h, from the later
of now and the current expiry, never past 30 days from now) or changes the
idle timeout; its creator (the `isb.owner` the caller would get) or the
org's admins may. `sandbox_list` shows each instance's kind (`sandbox`,
`workspace`, `replica`, `build`), creator, age, expiry, idle timeout, last
activity, limits and use, and whether the caller made it (`mine`).

**The reaper** in the daemon checks every minute and deletes a sandbox past
either deadline, recording `sandbox.reaped` in the history with the reason.
It only takes instances that carry isb's deadlines and are neither a
workspace, a stack replica nor a build, re-reads each before deleting it,
skips one with a terminal open, and leaves orgs placed on servers to their
agents. Sandboxes made by `isb create` and `isb up` on the host have no
deadlines and are never reaped.

The workspace cannot be reached as a sandbox: `sandbox_create` over its
name and `sandbox_remove` of it are refused.

## The tools

| Tool | Who | Does |
|---|---|---|
| `workspace_get` | members | the workspace (or `null`) and the org's settings: definition, status, resources, home, live sessions, last activity, token metadata, `connect` (`url`, `mcp_url`), sandbox count |
| `workspace_list` | members | every workspace in the org (one, unless `max_workspaces` was raised) |
| `workspace_create` | admins | `image`, `name`, `user`, `cpus`, `memory`, `root_size`, `home_size`, `env`, `secrets`, `labels`, `token_role`; `home_bind` (superadmins) |
| `workspace_update` | admins | any of those but `name`, `user`, `home_bind`; resizing needs `confirm` |
| `workspace_start`, `workspace_stop`, `workspace_restart` | members | stop and restart need `confirm` |
| `workspace_rebuild` | admins | `image`, `confirm` |
| `workspace_delete` | admins | `keep_home`, `confirm` |
| `workspace_token_rotate` | admins | a new token, delivered; the old one revoked |
| `workspace_settings` | members read, admins change | `max_workspaces` (platform admins), `sandbox_expiry`, `sandbox_idle` |
| `sandbox_create` | members | gains `expires`, `idle_timeout` |
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

## Migrating a box

`home_bind` (superadmins only: the unix socket, a superadmin token or
identity) mounts a host directory as the home instead of a volume, for a
trial period while a titan-iac box moves over; it must be under the org's
`--bind-root` directories, which incus enforces. The lasting form is a
volume: copy the old home in once, with the box stopped.

## Files

Under the org's state directory (`<state>/workspaces/` for the default org,
`<state>/orgs/<org>/workspaces/` for the others), 0600: `<name>.json` (the
definition, the token's id and hash), `<name>.token.age` (the token,
encrypted to the daemon's key), `settings.json`.
