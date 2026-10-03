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

Each org has four sections:

- **Overview** (`/orgs/ORG`): its stacks and a live activity feed.
- **Members** (`/orgs/ORG/members`): who is in the org, their role and when
  they were last active. Owners and admins also invite people (a link,
  shown once), change roles, remove members, make a new link for or revoke a
  pending invitation, and see and revoke every API token in the org. The
  role choices follow the server's rules: an admin hands out member and
  admin, only an owner touches an owner, and the last owner stays. Anyone
  can leave.
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

Platform admins also get **Platform** (`/admin/orgs`, `/admin/users`,
`/admin/server`): every org (create, delete), every user (disable, enable,
make or unmake platform admin) and the server's status.

The UI hides what a role may not do; the server decides ([auth.md](auth.md),
[orgs.md](orgs.md)), and its refusals are shown as it words them.

Light, dark and system themes; it works down to phone width.

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

Run a daemon with a listener, then the Vite dev server, which proxies `/api`
and `/healthz` to it and reloads on every edit. Build and run both inside a
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
