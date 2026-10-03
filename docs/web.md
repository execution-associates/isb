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
  memberships, or every org for a platform admin) and your account menu
  (account, theme, sign out). Each org's overview shows its projects with
  their health, the latest deployments of every app, its stacks and a live
  activity feed. **Account** changes your password, links and unlinks
  providers, adds and deletes passkeys, makes and revokes API tokens (shown
  once), and lists your sessions. Org owners and admins invite people from
  the org overview.

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
- **Monitoring**: CPU and memory per replica (the daemon's last 40 CPU
  samples, 2 seconds apart; memory as seen while the tab is open), health,
  rotation, restarts.
- **Terminal**: a login shell in a replica, in xterm.js (loaded only on this
  tab), over the daemon's terminal websocket (below).
- **Advanced**: named volumes, published ports, and deleting the app (typed
  confirm).

"New app" (from an environment) takes an image, or a repository with its
access (public, an HTTPS token secret, an existing SSH key secret, or a new
deploy key, which it generates and shows before the first deploy) and a
builder; templates come later. With "deploy right away" it opens the
deployment's live log.

Every page follows the event feed over one shared connection and refetches
what an event in its org touches.

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
