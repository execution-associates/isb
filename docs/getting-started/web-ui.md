---
title: The web UI
description: A tour of the browser UI that isb serve ships in its binary: orgs, workspaces, apps, deployments, databases, backups, members, MCP and the platform pages.
order: 5
nav_title: Web UI
---

`isb serve` serves a browser UI on its HTTP listener (`--listen`, by default
`127.0.0.1:8092`), next to the API it is built on. It is in the same binary,
needs nothing installed, and does nothing the API does not: the UI hides
what your role may not do, and the server decides. Open the listener's
address (or the public URL in front of it) in a browser.

Four themes: Execution Associates (the default, dark, in the colours and
type of executionassociates.com), light, dark and system. The choice is kept
per browser, from the account menu, the command palette or the sign-in
pages. The UI works down to phone width.

## Signing in

- **First run**: `/setup` creates the platform admin with the one-time setup
  token from `<state>/setup-token` ([the first admin](../guides/sign-in.md#the-first-admin)).
  The token may also come in the address, `/setup#TOKEN`.
- **Sign in** (`/login`): email and password, a passkey, and a button for each
  provider the daemon has configured (GitHub, Google, OIDC SSO). A failed
  provider sign-in comes back as `/login?error=CODE` and is explained in plain
  words; `?next=/path` is where you land afterwards (a path on this site
  only, as the server requires).
- **Accounts**: `/invite#TOKEN` accepts an invitation (a new account picks a
  password; an existing one confirms it, or accepts while signed in as that
  address). `/signup` offers provider sign-up when the operator turned on
  open sign-up, and otherwise says accounts are by invitation.
  `/forgot-password` makes a reset link (written to the daemon's log while no
  mailer is configured) and `/reset-password#TOKEN` sets the new password.

Tokens in these links sit in the URL fragment, which browsers never send to
a server, and the page drops them from the address bar once used.

A tailnet or Cloudflare Access superadmin needs no sign-in: the app opens
signed in as its isb user, or as its source when it has no account (then
Account shows only how it is signed in, and there is no Sign out, since the
next request would sign it in again). See [superadmins](../concepts/access.md#superadmins).

## Finding your way

- **The sidebar**: the Execution Associates wordmark and `isb`, which go
  home, an org switcher (the orgs you can open: your
  memberships, or every org for a platform admin; switching keeps the
  section you are in), a search button, the selected org's sections (**Org**:
  Overview, Projects, Workspace, Templates, Backups, Notifications;
  **Manage**: Members, MCP, Secrets, Settings, History), **Platform** for
  platform admins (and **Host** for superadmins), and your account menu
  (account, theme, sign out). The page sits in a panel with a top bar that
  holds its breadcrumbs (the last two on a phone) and the state of the live
  event stream.
- **Command palette** (⌘K or Ctrl+K, or `/`): every section, project
  environment and app of the org, the other orgs, platform pages, account,
  theme and sign out, matched as you type (a word that starts with what you
  typed ranks first; letters in order still match). Typing "deploy" lists
  Deploy/Redeploy for each app (writers only), which opens the deployment
  live. In the web terminal, Ctrl+K belongs to the shell; ⌘K still opens the
  palette.
- **Go to**: `G` then a letter jumps to a section from anywhere outside a
  text field: `G W` Workspace, `G O` Overview, `G P` Projects, `G T`
  Templates, `G B` Backups, `G N` Notifications, `G M` Members, `G A` MCP,
  `G S` Secrets, `G ,` Settings, `G H` History.
- **Account** (`/account`) changes your password, links and unlinks
  providers, adds and deletes passkeys and SSH keys ([SSH](../guides/ssh.md)),
  makes and revokes API tokens (shown once), and lists your sessions. A new
  API token picks an org, an expiry and an **Access**: full (the role's
  reach, what an agent gets by default), deploy, read only, or only some
  tools (globs); see [scopes](../concepts/access.md#scopes).

Every page follows the event feed over one shared connection and refetches
what an event in its org touches.

## An org's sections

- **Workspace** (`/orgs/ORG/workspace`): the org's machine and its
  sandboxes ([below](#the-workspace)).
- **Overview** (`/orgs/ORG`): the workspace first (status, live sessions,
  CPU and memory with a sparkline, last activity, sandbox count; or a
  "Create the workspace" button for admins), then its projects with their
  health, the latest deployments of every app, its stacks and a live
  activity feed.
- **Projects** (`/orgs/ORG/projects`): projects, their environments and the
  apps in each, with every app's pages ([below](#projects-and-apps)).
- **Templates** (`/orgs/ORG/templates`): the template catalog, what the org
  deployed from it, and for platform admins the catalogs
  ([below](#templates)).
- **Backups** (`/orgs/ORG/backups`): backup destinations, every database's
  and volume's backup schedules, and the restore history. It links to
  **Volumes**.
- **Volumes** (`/orgs/ORG/volumes`): the org's named volumes; one volume's
  page is the **Volume panel**: snapshots with Snapshot now, the schedule
  and pre-snapshot hook, the volume's backups with their files, and staged
  restores with Discard. Admins and owners act; members and viewers read
  ([volumes](../guides/volumes.md)).
- **Notifications** (`/orgs/ORG/notifications`): notification channels, their
  rules and delivery logs ([below](#notifications)).
- **Members** (`/orgs/ORG/members`): who is in the org, their role and when
  they were last active. Owners and admins also invite people (a link,
  shown once), change roles, remove members, make a new link for or revoke a
  pending invitation, and see and revoke every API token in the org (with
  its scopes). The role choices follow the server's rules: an admin hands
  out viewer, member and admin, only an owner touches an owner, and the
  last owner stays. Anyone can leave. A viewer sees the org but changes
  nothing; the server refuses what the UI still offers them.
- **MCP** (`/orgs/ORG/agents`; `/orgs/ORG/mcp` is the endpoint itself):
  how to connect an agent. The org endpoint's URL (`/orgs/ORG/mcp` on the
  origin the page is open at), a form that makes an API token for the org
  (name, Access, expiry; the server's rules: members, viewers with a
  read-only reach, platform admins; not an accountless superadmin or a
  narrowed token) and shows it once, and install snippets with copy buttons
  for Claude Code (`claude mcp add` and `.mcp.json`), Codex (`codex mcp add`
  and `config.toml`), Cursor and other `mcpServers` clients, and a `curl`
  `tools/list` test. The snippets read the token from `ISB_TOKEN`, and the
  token just made fills the `export` line. A switch adds Cloudflare Access
  service token headers for an address behind Access (on by default for one
  that is not localhost or the tailnet). The tools the endpoint lists (`GET
  /api/v1/tools`) fold out below, and a note says claude.ai and Claude
  Desktop connectors need Access Managed OAuth
  ([remote access](../guides/remote-access.md#cloudflare-tunnel-and-access)).
  A superadmin also sees the unbound `/mcp` endpoint, with a warning that it
  is root on the host, and for each source (a token minted on the host with
  `isb token create NAME --superadmin`, a `--superadmin-tailnet` identity
  with the allow list and tailnet URL from `host_policy`, a
  `--superadmin-access` identity) whether it is on and its snippets; the web
  makes no superadmin token. `/agents` opens the remembered org's page, or
  the superadmin part alone when there is no org. See
  [agents and MCP](../guides/agents.md).
- **Secrets** (`/orgs/ORG/secrets`): the org's secrets with driver, version,
  update time, labels and the stacks using each, plus the driver references
  stacks read (refresh one to check it now). Create one or give it a new
  value from a password-style field or a file (sent once, never shown back),
  delete one (the server's refusal is shown while a stack uses it), and set
  up 1Password (the `onepassword-token` secret and how to write references).
  Values never reach the page except through **Reveal**: owners and admins
  only, after a confirmation, through `secret_get`, and hidden again after 30
  seconds. Members can read values through the API anyway (the org is the
  trust boundary); the UI keeps them off screen. See [secrets](../guides/secrets.md).
- **Settings** (`/orgs/ORG/settings`): quota and per-instance defaults,
  network (bridge, subnet, service-name domain, bind roots) and egress
  exceptions, and the org's placement and isolation. Platform admins edit
  the quota and egress exceptions and delete the org (typing its name; for
  an org in a dedicated VM, a switch deletes the VM too); everyone else sees
  them read-only. Moving an org is not supported, and the page says so with
  the manual procedure ([moving an org](../concepts/placement.md#moving-an-org)).
- **History** (`/orgs/ORG/history`, every member): what happened in the org
  ([history](../operations/history.md)): the controller's events, incus
  changes made through isb or not (with who requested them), and, for owners
  and admins, the audit log ([audit](../operations/audit.md)). A source
  filter, object, kind and actor filters (globs), a time range, older
  entries on demand, a live tail, each entry's details (and the likely cause
  of an incus change) on a click, and **Export JSONL** of everything
  matching. `/orgs/ORG/audit` opens it on the audit log.

## The workspace

`/orgs/ORG/workspace[/TAB]`, over the `workspace_*` tools
([workspaces](../concepts/workspaces.md)). With no workspace yet, the page is
the create form: image (suggestions `dev-base`, `images:ubuntu/24.04`),
name, user, CPUs, memory, root size, home size, the token's role (viewer,
member, admin; admin by default, with what each grants) and environment,
with the org's placement (where it runs, its project, network and limits)
shown read-only. Members and viewers see the form read-only with why.

The header shows the status, image, user, live sessions and sandbox count.
Members get Start, Stop and Restart; admins also Rebuild (type the name) and
Delete (type the name; a switch keeps the home volume). Each disruptive
action first asks the daemon without `confirm` and shows its answer, the
live sessions it would end, in the dialog; confirming calls again with
`confirm: true`. The tabs:

| Tab | Shows |
|---|---|
| **Terminal** | Shells as tabs: **New** opens another as the workspace user in its home; tabs you are not looking at stay connected, and closing one ends its shell. A sandbox's **Shell** (Sandboxes tab) opens as its own tab, as root. Leaving or reloading the page ends its shells: there is no reattach. Not for viewers. |
| **Connect** | The workspace's MCP credential: role, created, last used, path inside (`/run/isb/token`), id, audit actor `workspace`, and Rotate for admins (no token value is ever shown); the variables login shells get; MCP client snippets for use inside the workspace (`$ISB_URL/orgs/ORG/mcp`, `$ISB_TOKEN`); SSH and herdr: `isb key add`, `isb workspace ssh-config`, `ssh NAME.ORG.isb` and the `herdr machine add` line ([SSH](../guides/ssh.md)). |
| **Resources** | CPU, memory, disk, address, last activity, sessions, a CPU sparkline; admins resize CPUs, memory and the root disk (confirmed). |
| **Home** | The volume, pool and its driver, size, mount path (or the host folder); admins grow it (confirmed). For a volume home, the Volume panel: snapshots, backups and staged restores, with a warning on a pool where every snapshot is a full copy. |
| **Environment** | `KEY=VALUE` variables for login shells (`ISB_*` refused) and the org secrets delivered as files; admins save, which delivers them again. |
| **Sandboxes** | Each sandbox: status, creator, age, expiry (highlighted in its last hour), idle limit, last activity, limits and use; Shell, Extend by 4h, 24h or 7d (its creator, or admins) and Delete; the org's expiry and idle defaults. |
| **History** | The history panel, filtered to the workspace. |

Viewers see Connect, Resources, Home, Environment, Sandboxes and History,
without actions.

## Projects and apps

The pages over [apps](../guides/deploy-apps.md), in Dokploy's layout:

| Page | Shows |
|---|---|
| `/orgs/<org>/projects` | The org's projects as cards: environments with their app counts, and health (the worst of the environments' stacks). New project (name, description, environments). |
| `/orgs/<org>/projects/<project>/<env>` | The project's environments as tabs; the environment's apps with state, source (image and digest, or repository, branch and commit), replicas, domains and last deploy. New app, add an environment, delete an empty environment or project (typed confirm). |
| `/orgs/<org>/apps/<app>/<tab>` | One app: state, Deploy (or Redeploy), Stop and Start, and the tabs below. |

An app's tabs:

- **General**: the source (image; or repository URL, branch, subdirectory,
  token or SSH key secret, submodules, and a deploy key to generate and
  copy), the build (builder, Dockerfile path and target, build arguments,
  VM or container), scale (applies at once: saved with `app_update`, then
  `stack_scale`), runtime (port, CPUs, memory, command), the health check,
  and the webhook URL with its secret (reveal, rotate). Each card saves on
  its own; settings take effect at the next deploy.
- **Environment**: the `.env` editor, with line numbers and highlighting,
  checked as the daemon parses it (errors block the save, warnings explain:
  a quoted `${{secret...}}` is literal text, a key set twice). Secret
  references show in violet, and in red when the org has no such secret.
  Save, or save and deploy.
- **Domains**: each domain with its URL, route state and certificate state
  from the ingress; add, edit and remove (host or `auto`, path, port,
  HTTPS, redirect, strip prefix, `www.` redirect), checked as the ingress
  checks them. Domains are routed at the next deploy; the tab says when
  some are not routed and offers the deploy.
- **Deployments**: the last 30 with status, trigger and caller, commit or
  image and digest, and duration; Roll back on earlier successful ones. A
  deployment's page follows it live: its status and stage (Queued, then
  Build, Pull or Restore, Roll out, Live) change in place, the clock ticks,
  and the log streams from its first line, without showing anything twice
  and with the status never lagging the text (a quiet feed still gets a
  pull every 3 seconds). The log has ANSI colours, error lines marked,
  find, wrap, copy and download, and follows the end while you are at the
  bottom (scrolling up pauses it; Follow resumes). When it ends, a success
  panel links the app's URL (and offers to roll back to it once it is no
  longer current), and a failure panel says which step failed and offers
  Deploy again, Roll back to the last good deployment and a link to it. A
  rail lists the recent deployments.
- **Deploying opens the deployment at once**: Deploy, Redeploy, Roll back
  (here, on a deployment or in the palette) and "New app" with "deploy
  right away" open the queued deployment's page in the same frame, which
  reads the log immediately. A deployment started elsewhere (a webhook
  push, a teammate, an agent) shows as a live bar under the app's header
  (stage, clock, newest log line) and as a toast with **Watch** anywhere in
  the org; on the Deployments tab it opens by itself, and a finished
  deployment's page moves on to the newer one.
- **Logs**: the replicas' recent output (`stack_logs`), per replica or all,
  refreshed every 5 seconds.
- **Monitoring**: the metrics history ([metrics](../operations/metrics.md))
  over 1 hour, 24 hours, 7 days or 30 days: CPU, memory, network in and out,
  disk read and write, summed over the replicas or one line per replica,
  with a readout on hover. About 240 points per chart; a missing bucket
  between two values is drawn through, longer gaps stay gaps. Above, the
  live CPU, memory, health and restarts; below, each replica's state now.
- **Jobs**: the app's scheduled jobs ([jobs](../guides/jobs.md)): command
  (split as the daemon splits it, with a preview of the argv), where it runs
  (a running replica or a one-off instance), the cron schedule with its next
  runs, time zone, timeout, what happens when a run is still going, runs
  kept, and optional user, working directory and environment. Each job
  shows its last run and next run, Run now, enable or disable, edit and
  delete, and its runs (trigger, status, start, duration, exit code), each
  with its log followed live.
- **Terminal**: a login shell in a replica, over the daemon's terminal
  websocket ([HTTP API](../reference/http-api.md#the-web-terminal)).
- **Advanced**: named volumes, published ports, and deleting the app (typed
  confirm).

A database app has **Database** and **Backups** tabs in place of General
and Domains; an app built from git also has **Previews**.

"New app" (from an environment) takes an image, or a repository with its
access (public, an HTTPS token secret, an existing SSH key secret, or a new
deploy key, which it generates and shows before the first deploy) and a
builder. With "deploy right away" it opens the deployment's live log. Its
**Database** choice opens the new-database dialog, and **Template** the
catalog, aimed at that environment.

## Databases, backups, jobs and previews

- **New database** (the project's menu, or New app → Database): engine
  (Postgres, MySQL, MariaDB, MongoDB, Redis), version (the official image's
  tag), name, and optionally the database and user names and a host port to
  publish on (`database_create`). It opens the database's page.
- **Database** tab: how to connect (`database_get`): host (the service
  name), port, user, database, the password as the secret it lives in, the
  URL inside the org with the password as a `${{secret.db.NAME.password}}`
  reference, the published URLs, the data volume, and the line to paste into
  an app, `DATABASE_URL=${{secret.db.NAME.url}}`; each with a copy button.
  **Reveal** (members and up; viewers are refused by the server) calls
  `database_get` with `reveal: true`, shows the password and the full URL,
  and hides them again after 30 seconds; the read is in the audit log.
- **Backups** tab: the database's schedules, each with its destination, the
  schedule in words and its next run, keep and compression; **Back up now**
  (its log opens and follows), pause or resume, edit, delete, restore the
  newest; its runs (status, size against the dump's size, duration, log)
  and the files in its bucket, each with **Restore**. A restore goes into a
  new database beside this one (named, with credentials of its own), or into
  this database, which replaces its data and asks for its name typed first.
  Restores into or from this database are listed with their logs. See
  [databases](../guides/databases.md).
- **Backups** section: destinations (endpoint, with buttons for AWS S3, R2,
  B2 and self-hosted stores, region, bucket, prefix, path-style URLs, the key
  pair typed in (stored as `backup.NAME.*` secrets) or existing secrets,
  create the bucket), tested when added and on **Test**; every schedule in
  the org; the restore history.
- **Schedules** (backups, jobs and snapshots) are cron as the daemon reads
  it: presets, the schedule in words, the next three runs in UTC or the
  given offset, and the parser's own errors before saving.
- **Previews** tab (apps built from git): the live previews with pull
  request, branches, head commit, URL, status, last deployment's error,
  **Log**, **Redeploy** and **Delete**; and the settings saved with
  `app_update` (`previews`): on or off, base branches, at most N, replicas,
  domain (`auto` or `*.suffix`), port, remove after idle, a commit status
  token secret, start from the app's environment, the preview environment
  (the same editor as the Environment tab), and previews for forks, with a
  warning and the secrets a fork may receive ticked one by one. See
  [previews](../guides/previews.md).

## Notifications

Each channel with its destination (secret names only), rules in words, an
on/off switch, **Test** (sends one and says how it went), edit, delete, and
its delivery log (status, attempts, HTTP status, error). The channel dialog
takes webhook (URL and signing secrets), Slack, Discord, Telegram (bot token
secret, chat id) or email (SMTP server, TLS, port, username, password
secret, from, to), with secret names suggested from the org's and flagged
when missing; and rules, each a set of event kinds ticked by subject
(deployments, health, backups, restores, jobs, certificates, previews) plus
optional project, app and stack filters. Ticks are written back as the
shortest globs (`*`, `deploy.*`, `*.failed`); a saved glob the page does not
know is kept. Platform admins also see the server-wide switch for private
destinations. See [notifications](../guides/notifications.md).

## Templates

The catalog with search and tags; cards show a template's logo, served from
isb's cached copy ([logos](../guides/templates.md#logos)), so the page makes
no requests to other servers. A template without a logo, or whose logo fails
to load, shows its initials. A template's page lists what it creates, its
links and notes (and a Dokploy template's translation notes or refusals),
and a form generated from its variables: each typed (email, URL, number,
domain, choices) and checked as the daemon checks it, generated ones left
empty to be generated, secret ones as password fields. **Preview plan** is
a dry run (apps in order, where each value came from, the secrets it
creates, URLs, anything in the way); **Deploy** creates the apps and opens
the first app's deployment live; when it is done, the page moves on to the
next app's deployment as it is queued, in dependency order. What the org
deployed is listed with its apps and URLs, and removing an instance deletes
its apps and `tpl.NAME.*` secrets. Platform admins manage catalogs there too
(add a directory or https URL in isb's or Dokploy's format, remove one), with
a one-click fill for Dokploy's. See [templates](../guides/templates.md).

## Platform

Platform admins get **Platform** (`/admin`, with tabs Orgs, Users, This
host, Servers and History: `/admin/orgs`, `/admin/users`, `/admin/server`,
`/admin/servers`, `/admin/history`): every org (create, delete), every user
(disable, enable, make or unmake platform admin), this host's status, the
servers, and the whole history (every org and the host, host-level rows
only, or one org).

- **Orgs** shows where each org runs (this host, a server, or a dedicated
  VM). **New org** asks where it should run, each choice with what keeps the
  org apart: this host (an incus project; containers share the host's
  kernel), each server that is up (another machine), or a dedicated VM (its
  own kernel; with the VM's CPUs, memory and disk), disabled with the reason
  when this host cannot run VMs ([placement](../concepts/placement.md)). A
  dedicated VM takes minutes: the dialog follows its steps and log and
  offers Retry on a failure. The org's quota and egress are set alongside.
- **Servers**: each server with its kind (added over SSH, or a dedicated VM
  and its org), health (up, unreachable, unknown, and how long since its
  last heartbeat), isb version, CPU, memory and disk from the heartbeat, and
  the orgs on it; servers being added, with their progress. A server's
  details (address, how it was added, certificate fingerprint and expiry,
  firewall sources, last error) open from its row, with **Remove**, offered
  only while no org is placed on it (a dedicated VM is deleted with it).
  **Add server** is a wizard over `server_add`: name, `user@host`, the SSH
  private key (pasted, sent once, never stored, cleared from the form on
  submit), the addresses allowed to reach the agent port (prefilled with the
  addresses this control plane's traffic leaves from), and the binary to
  install (this version's release, the control plane's own build, or another
  release). It then follows the bootstrap step by step and, on a failure,
  shows the error with **Retry**, which keeps everything but the key. See
  [servers](../guides/servers.md).

## Host (superadmins)

[Superadmins](../concepts/access.md#superadmins) carry a **Superadmin**
badge in the top bar (its tooltip says how they are signed in) and get
**Host** (`/host`, `/host/policy`, `/host/superadmins`):

- **Instances**: every incus project (isb orgs marked) and every container
  and VM on the host, isb's or not, with project, org, image, address, isb's
  stack and owner labels and status; filter by text or project.
- **Serve policy**: the listen addresses, socket, public URL, Access and
  `--allow-unauthenticated`, and what remote callers' specs may ask for
  (privileged, raw, bind roots, publish addresses, any instance, the tool
  allow and deny lists), from `host_policy`.
- **Superadmins**: the sources that grant it (the socket, the token count,
  the `--superadmin-tailnet` and `--superadmin-access` lists, read-only:
  they are flags), and the superadmin tokens with last use and expiry, each
  revocable. Minting stays `isb token create NAME --superadmin` on the host;
  the page says so.

The UI offers no privileged, raw or bind-mount form fields to anyone;
superadmins use the tools for those.

## What viewers see

Viewers see an app's pages without Deploy, Start, Stop or the Terminal tab,
and the Projects pages without New project, New app and project actions.
Pages and tabs hide what a viewer may not do (create, edit, run, delete,
reveal); the server refuses it anyway, and its refusals are shown as it
words them ([roles](../concepts/access.md#roles)).

With Cloudflare Access configured, the UI sits behind it like every other
route; isb's own sign-in applies after it.

How the UI is built, embedded and served, and its security headers, are in
[developing the web UI](../contributing/web-ui.md).
