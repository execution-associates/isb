# The web UI

`isb serve` serves a browser UI on its HTTP listener (`--listen`), next to the
API it is built on. Open the listener's address (or the public URL in front
of it) in a browser:

- **First run**: `/setup` creates the platform admin with the one-time setup
  token from `<state>/setup-token` ([auth.md](auth.md#the-first-admin)).
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
- **Signed in**: a sidebar with an org switcher (the orgs you can open: your
  memberships, or every org for a platform admin; switching keeps the
  section you are in), the selected org's sections, and your account menu
  (account, theme, sign out). **Account** changes your password, links and
  unlinks providers, adds and deletes passkeys, makes and revokes API tokens
  (shown once), and lists your sessions.

Each org has these sections, and one more for its owners and admins:

- **Overview** (`/orgs/ORG`): its projects with their health, the latest
  deployments of every app, its stacks and a live activity feed.
- **Projects** (`/orgs/ORG/projects`): projects, their environments and the
  apps in each, with every app's pages (see below).
- **Templates** (`/orgs/ORG/templates`): the template catalog, what the org
  deployed from it, and for platform admins the catalogs (see
  [Day 2](#day-2-databases-backups-jobs-notifications-templates-previews)).
- **Backups** (`/orgs/ORG/backups`): backup destinations, every database's
  backup schedules, and the restore history.
- **Notifications** (`/orgs/ORG/notifications`): notification channels, their
  rules and delivery logs.
- **Members** (`/orgs/ORG/members`): who is in the org, their role and when
  they were last active. Owners and admins also invite people (a link,
  shown once), change roles, remove members, make a new link for or revoke a
  pending invitation, and see and revoke every API token in the org (with
  its scopes). The role choices follow the server's rules: an admin hands
  out viewer, member and admin, only an owner touches an owner, and the
  last owner stays. Anyone can leave. A viewer sees the org but changes
  nothing; the server refuses what the UI still offers them.
- **Secrets** (`/orgs/ORG/secrets`): the org's secrets with driver, version,
  update time, labels and the stacks using each, plus the driver references
  stacks read (refresh one to check it now). Create one or give it a new
  value from a password-style field or a file (sent once, never shown back),
  delete one (the server's refusal is shown while a stack uses it), and set
  up 1Password (the `onepassword-token` secret and how to write references).
  Values never reach the page except through **Reveal**: owners and admins
  only, after a confirmation, through `secret_get`, and hidden again after 30
  seconds. Members can read values through the API anyway (the org is the
  trust boundary); the UI keeps them off screen.
- **Settings** (`/orgs/ORG/settings`): quota and per-instance defaults,
  network (bridge, subnet, service-name domain, bind roots) and egress
  exceptions. Platform admins edit the quota and egress exceptions and
  delete the org (typing its name); everyone else sees them read-only.
- **Audit** (`/orgs/ORG/audit`, owners and admins): the org's audit log
  ([audit.md](audit.md)) newest first, filtered by actor, action and target
  (globs), outcome and time, with older entries on demand, a live tail over
  `/api/v1/audit/stream`, each entry's details (address, agent, request id,
  whitelisted arguments, its hash) on a click, and **Export JSONL** of
  everything matching.

Platform admins also get **Platform** (`/admin/orgs`, `/admin/users`,
`/admin/server`, `/admin/audit`): every org (create, delete), every user
(disable, enable, make or unmake platform admin), the server's status, and
the whole audit log (every org, platform-level entries only, or one org).

New API tokens (Account) pick an org, an expiry and an **Access**: full (the
role's reach, what an agent gets by default), deploy, read only, or only
some tools (globs); see [auth.md](auth.md#scopes).

The UI hides what a role may not do; the server decides ([auth.md](auth.md),
[orgs.md](orgs.md)), and its refusals are shown as it words them.

Light, dark and system themes; it works down to phone width.

## Projects and apps

The pages over [apps](apps.md), Dokploy's layout:

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
  some are not routed yet and offers the deploy.
- **Deployments**: the last 30 with status, trigger and caller, commit or
  image and digest, and duration; Roll back on earlier successful ones.
  A deployment's page follows its log live: each log line on the event
  feed pulls the new text by offset, so nothing shows twice; it follows
  the end while you are at the bottom and pauses when you scroll up. A
  failure is shown above the log with what to do next.
- **Logs**: the replicas' recent output (`stack_logs`), per replica or all,
  refreshed every 5 seconds.
- **Monitoring**: the metrics history (`metrics_query`,
  [metrics.md](metrics.md)) over 1 hour, 24 hours, 7 days or 30 days: CPU,
  memory, network in and out, disk read and write, summed over the replicas
  or one line per replica, with a readout on hover. About 240 points per
  chart; a missing bucket between two values is drawn through, longer gaps
  stay gaps. Above, the live CPU, memory, health and restarts; below, each
  replica's state now. The charts are plain SVG (no chart library).
- **Jobs**: the app's scheduled jobs ([jobs.md](jobs.md)): command (split
  as the daemon splits it, with a preview of the argv), where it runs (a
  running replica or a one-off instance), the cron schedule with its next
  runs, time zone, timeout, what happens when a run is still going, runs
  kept, and optional user, working directory and environment. Each job
  shows its last run and next run, Run now, enable or disable, edit and
  delete, and its runs (trigger, status, start, duration, exit code), each
  with its log followed live.
- **Terminal**: a login shell in a replica, in xterm.js (loaded only on this
  tab), over the daemon's terminal websocket (below).
- **Advanced**: named volumes, published ports, and deleting the app (typed
  confirm).

A database app has **Database** and **Backups** tabs in place of General
and Domains; an app built from git also has **Previews** (below).

"New app" (from an environment) takes an image, or a repository with its
access (public, an HTTPS token secret, an existing SSH key secret, or a new
deploy key, which it generates and shows before the first deploy) and a
builder. With "deploy right away" it opens the deployment's live log. Its
**Database** choice opens the new-database dialog, and **Template** the
catalog, aimed at that environment.

Every page follows the event feed over one shared connection and refetches
what an event in its org touches.

## Day 2: databases, backups, jobs, notifications, templates, previews

- **New database** (the project's menu, or New app → Database): engine
  (Postgres, MySQL, MariaDB, MongoDB, Redis), version (the official image's
  tag), name, and optionally the database and user names and a host port
  to publish on (`database_create`). It opens the database's page.
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
  and the files in its bucket (`backup_list` with a name), each with
  **Restore**. A restore goes into a new database beside this one (named,
  with credentials of its own), or into this database, which replaces its
  data and asks for its name typed first. Restores into or from this
  database are listed with their logs.
- **Backups** section: destinations (endpoint, with buttons for AWS S3, R2,
  B2 and self-hosted stores, region, bucket, prefix, path-style URLs, the key
  pair typed in (stored as `backup.NAME.*` secrets) or existing secrets,
  create the bucket), tested when added and on **Test**; every schedule in
  the org; the restore history.
- **Schedules** (backups and jobs) are cron as the daemon reads it
  (`web/src/lib/cron.ts` mirrors `src/cron.rs`, with its tests): presets,
  the schedule in words, the next three runs in UTC or the given offset, and
  the parser's own errors before saving.
- **Notifications** section: each channel with its destination (secret
  names only), rules in words, an on/off switch, **Test** (sends one and
  says how it went), edit, delete, and its delivery log (status, attempts,
  HTTP status, error). The channel dialog takes webhook (URL and signing
  secrets), Slack, Discord, Telegram (bot token secret, chat id) or email
  (SMTP server, TLS, port, username, password secret, from, to), with secret
  names suggested from the org's and flagged when missing; and rules, each a
  set of event kinds ticked by subject (deployments, health, backups,
  restores, jobs, certificates, previews) plus optional project, app and
  stack filters. Ticks are written back as the shortest globs (`*`,
  `deploy.*`, `*.failed`); a saved glob the page does not know is kept.
  Platform admins also see the server-wide switch for private destinations
  (`notification_settings`).
- **Templates** section: the catalog (`template_list`) with search and
  tags; cards show a template's initials, not its logo (logos are
  third-party URLs: the CSP keeps `img-src` to this origin, and the page
  makes no requests to other servers). A template's page lists what it
  creates, its links and notes (and a Dokploy template's translation notes
  or refusals), and a form generated from its variables: each typed
  (email, URL, number, domain, choices) and checked as the daemon checks
  it, generated ones left empty to be generated, secret ones as password
  fields. **Preview plan** is a dry run (apps in order, where each value
  came from, the secrets it creates, URLs, anything in the way);
  **Deploy** creates the apps and opens the main app's deployments. What
  the org deployed is listed with its apps and URLs, and removing an
  instance deletes its apps and `tpl.NAME.*` secrets. Platform admins
  manage catalogs there too (add a directory or https URL in isb's or
  Dokploy's format, remove one), with a one-click fill for Dokploy's.
- **Previews** tab (apps built from git): the live previews with pull
  request, branches, head commit, URL, status, last deployment's error,
  **Log**, **Redeploy** and **Delete**; and the settings saved with
  `app_update` (`previews`): on or off, base branches, at most N, replicas,
  domain (`auto` or `*.suffix`), port, remove after idle, a commit status
  token secret, start from the app's environment, the preview environment
  (the same editor as the Environment tab), and previews for forks, with a
  warning and the secrets a fork may receive ticked one by one.

Pages and tabs hide what a viewer may not do (create, edit, run, delete,
reveal); the server refuses it anyway.

## How it talks to the daemon

Only through the public HTTP API, like any other client:

- `/api/v1/auth/*` for identity ([auth.md](auth.md)), with the session cookie
  and `X-Isb-Csrf: 1` on every state-changing request.
- `POST /api/v1/tools/<tool>` (and `/orgs/<org>/api/v1/tools/<tool>`) for
  everything else ([serve.md](serve.md)). The client is typed from the
  daemon's OpenAPI document: `web/openapi.json` is a snapshot of
  `/api/v1/openapi.json`, and `bun run gen:api` turns it into
  `web/src/api/openapi.gen.ts`. A tool missing from the snapshot still works,
  untyped, so new tools need no change to the UI server.
- `GET /api/v1/events` (server-sent events) for live updates, reconnecting
  with backoff (1 s doubling to 30 s) and resuming after the last event seen.
- `GET /orgs/<org>/api/v1/terminal` (a websocket) for the Terminal tab
  ([serve.md](serve.md#the-web-terminal)).

## Built, embedded, served

`web/` is React, TypeScript, Vite, Tailwind CSS and shadcn/ui components
(vendored in `web/src/components/ui`), with bun as the package manager and a
committed `bun.lock`.

```sh
cd web
bun install --frozen-lockfile
bun run build        # typecheck, then web/dist
bun run test         # vitest
bun run lint         # tsc
```

`cargo build` embeds `web/dist` in the binary: `build.rs` writes a table of
`include_bytes!` for every file in it, so the binary reads nothing from disk
at runtime, a request path can never reach the filesystem, and the static
musl build gains no dependency. The UI is optional at build time:

- **Without `web/dist`**, the table holds a one-page placeholder that says the
  UI was not built (the API and MCP work as usual), and the daemon logs that
  at startup. A plain `cargo build`, `cargo test` or `cargo install` never
  needs bun.
- **`ISB_WEB_REQUIRED=1`** turns a missing `web/dist` into a build error.
  Release builds set it after building the UI, so a release never ships the
  placeholder.
- `ISB_WEB_DIST=/path` embeds another directory.

Cargo reruns the build script when anything under `web/dist` changes, so
`bun run build` then `cargo build` picks up the new UI.

### What the server does with it

The UI's routes answer only what the API does not:

| Request | Answer |
|---|---|
| `/api/...`, `/mcp`, `/healthz`, `/orgs/<org>/mcp`, `/orgs/<org>/api/...` | never the UI: the API, or a 404 |
| a file in `web/dist` | the file, with its content type |
| `/assets/*` (Vite's content-hashed bundles) | `Cache-Control: public, max-age=31536000, immutable` |
| `/index.html` and every client route | `index.html`, `Cache-Control: no-store`, so a new binary's UI loads at once |
| a missing `/assets/*` file, or any missing path with a file extension | 404, never the HTML shell |
| any method but `GET` and `HEAD` | 405 |

Every UI response carries:

```text
Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
  img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none';
  base-uri 'none'; form-action 'self'; frame-ancestors 'none'
X-Frame-Options: DENY
X-Content-Type-Options: nosniff
Referrer-Policy: same-origin
Cross-Origin-Opener-Policy: same-origin
Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=()
```

Scripts come only from this origin and none are inline (the theme is applied
before first paint by `/theme.js`, a file, for that reason); fetches and the
event stream go only to this origin. Inline *styles* are allowed: the dialog
and toast components inject `<style>` elements at runtime, and a style cannot
run code. Tokens in links (`/invite#...`, `/reset-password#...`,
`/setup#...`) sit in the URL fragment, which browsers never send to a server
or in a `Referer`, and the page drops them from the address bar once used.

With Cloudflare Access configured, the UI sits behind it like every other
route; isb's own sign-in applies after it.

## Developing

Run a daemon with a listener, then the Vite dev server, which proxies `/api`,
`/orgs/<org>/api` (websockets included) and `/healthz` to it and reloads on
every edit. Build and run both inside a
sandbox per the repository's rules; the daemon needs the incus socket, so run
the binary you built there on the host:

```sh
ISB_PUBLIC_URL=http://localhost:5173 isb serve --listen 127.0.0.1:8092 ...
cd web && ISB_URL=http://127.0.0.1:8092 bun run dev    # http://localhost:5173
```

`ISB_PUBLIC_URL` must be the page's origin for passkeys (the relying party
checks it) and for provider callbacks. Session cookies are not `Secure` over
plain loopback HTTP, so `http://localhost` works.

To refresh the typed client after adding or changing tools:

```sh
curl -s http://127.0.0.1:8092/api/v1/openapi.json | jq . > web/openapi.json
cd web && bun run gen:api
```

Add shadcn components with `bunx shadcn@latest add NAME` in `web/`
(`components.json` is set up), then check what it changed: imports should use
`@/lib/utils`, and no new runtime dependency should appear without a reason.
