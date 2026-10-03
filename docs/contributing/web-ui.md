---
title: Developing the web UI
description: How the web UI is built, how it talks to the daemon, how it is embedded in and served by the isb binary, its design system, and the development loop.
order: 1
nav_title: Web UI
---

The web UI is a React app in `web/` that `cargo build` embeds in the isb
binary, so a release ships one file and the UI needs no server of its own. It
uses only the daemon's public API, the same one agents and the CLI use, so
anything the UI can do an API client can do too. What the UI looks like to its
users is in [The web UI](../getting-started/web-ui.md).

## How it talks to the daemon

Only through the public HTTP API, like any other client:

- `/api/v1/auth/*` for identity ([Identity API](../reference/identity-api.md)),
  with the session cookie and `X-Isb-Csrf: 1` on every state-changing
  request.
- `POST /api/v1/tools/<tool>` (and `/orgs/<org>/api/v1/tools/<tool>`) for
  everything else ([HTTP API](../reference/http-api.md)). The client is typed
  from the daemon's OpenAPI document: `web/openapi.json` is a snapshot of
  `/api/v1/openapi.json`, and `bun run gen:api` turns it into
  `web/src/api/openapi.gen.ts`. A tool missing from the snapshot still works,
  untyped, so new tools need no change to the UI server.
- `GET /api/v1/events` (server-sent events) for live updates, reconnecting
  with backoff (1 s doubling to 30 s) and resuming after the last event seen.
  Every page follows the event feed over one shared connection and refetches
  what an event in its org touches.
- `GET /orgs/<org>/api/v1/terminal` (a websocket) for terminals
  ([HTTP API](../reference/http-api.md#the-web-terminal)).

The UI hides what a role may not do; the server decides, and its refusals are
shown as it words them.

## Built, embedded, served

`web/` is React, TypeScript, Vite, Tailwind CSS and shadcn/ui components
(vendored in `web/src/components/ui`), with bun as the package manager and a
committed `bun.lock`.

```sh
cd web
bun install --frozen-lockfile
bun run build        # typecheck, then web/dist
bun run test         # vitest
bun run lint         # tsc, then oxlint
bun run oxlint       # oxlint alone (well under a second)
```

[oxlint](https://oxc.rs/docs/guide/usage/linter) checks correctness and
suspicious code, React and its hooks (`rules-of-hooks`, `exhaustive-deps`),
accessibility, imports (`no-cycle`, `no-duplicates`), vitest and promises; it
has no style or size rules. `web/.oxlintrc.json` holds the configuration, with
a reason beside every rule it turns off or tunes. Warnings fail the run, and so
does a disable comment that no longer suppresses anything. An inline disable
names its rule and says why after `--`:

```ts
// oxlint-disable-next-line react/refs -- the follower is a mutable buffer; `bump` re-renders after every change to it
```

CI runs `bun run lint`, `bun run test` and `bun run build`.

`cargo build` embeds `web/dist` in the binary: `crates/isb-server/build.rs`
writes a table of `include_bytes!` for every file in it, so the binary reads
nothing from disk at runtime, a request path can never reach the filesystem,
and the static musl build gains no dependency. The UI is optional at build
time:

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
| a file in `web/dist` | the file, with its content type; `Cache-Control: public, max-age=3600` |
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
run code. Images come only from this origin too, which is why template logos
are served from isb's own cached copy
([Templates](../guides/templates.md#logos)). Tokens in links
(`/invite#...`, `/reset-password#...`, `/setup#...`) sit in the URL fragment,
which browsers never send to a server or in a `Referer`, and the page drops
them from the address bar once used.

With Cloudflare Access configured, the UI sits behind it like every other
route; isb's own sign-in applies after it.

## Design system

- **Themes**: Execution Associates (the default when no choice is saved),
  light, dark and system. The choice is `isb-theme` in `localStorage`;
  `web/public/theme.js` applies it before the first paint and
  `web/src/lib/theme.ts` afterwards, and the two must agree. The Execution
  Associates theme takes executionassociates.com's palette and type: ink
  surfaces, white text, peach focus rings, glow pink as the brand colour,
  Geist and Geist Mono, Archivo (widened) for page titles and sidebar labels,
  square-ish 4 px corners, and the site's coast at night on the sign-in panel.
  The site is dark only, so the theme is dark only: `<html>` carries `dark`
  and `ea`, and `:root.ea` replaces the dark palette.
- **Tokens** (`web/src/index.css`): in light and dark, neutrals with a faint
  cool tint and emerald as the one brand colour (active navigation, switches,
  meters); in every theme, semantic status colours: success (running, done),
  info (building, deploying), warning (degraded), destructive (failed);
  queued and stopped are neutral. Log panels use a dark terminal surface in
  every theme. Text and controls meet WCAG 2.1 AA contrast in each theme.
- **Logo**: the full Execution Associates wordmark beside the product name
  `isb` is the home link (sidebar, sign-in pages); the EXA monogram stands in
  where the wordmark does not fit (the phone top bar). Both are rasters
  downscaled from the marketing site's files
  (`web/src/assets/brand/exa-lockup@{1,2,3}x.png`,
  `exa-mark@{1,2,3}x.png`) drawn as CSS masks filled with `currentColor`
  (`.exa-lockup`, `.exa-mark`; `<Lockup>`, `<Logo>`, `<Wordmark>`), so one
  asset reads on every theme. The page title is `isb`. The favicons and
  `apple-touch-icon.png` in `web/public` are the monogram, white on a dark
  tile.
- **Status** goes through `lib/status.ts` (status to tone, tone to classes)
  and `<StatusBadge>`/`<StatusDot>` (`components/status.tsx`); a pulsing dot
  means in progress or live. No page picks status colours by hand.
- **Type**: Inter for text and JetBrains Mono for identifiers and logs in
  light and dark; Geist, Geist Mono and Archivo in the Execution Associates
  theme (all SIL OFL 1.1, from `@fontsource-variable`, Latin subsets served
  from the binary, since the CSP allows no font CDN; `web/src/fonts.css`).
  Page titles use `font-display`, which is the text face except in the
  Execution Associates theme. Page titles 20-24 px semibold, section titles
  15 px, body 13-14 px, numbers tabular.
- **Patterns**: a page header (title with badges, one-line description,
  actions that wrap under it on phones), breadcrumbs declared by the page and
  drawn in the top bar, settings sections with their own Save, empty states
  that name the next action, skeletons shaped like what loads, typed
  confirmation for destructive actions, toasts for outcomes.
- **Motion** is short and optional: content fades up as it appears, live dots
  pulse; `prefers-reduced-motion` turns animation off.
- **Charts** (the Monitoring tab) are plain SVG, with no chart library.
- **Schedules** are parsed in the page exactly as the daemon parses them:
  `web/src/lib/cron.ts` mirrors `crates/isb-core/src/cron.rs`, with its
  tests.
- Every page works down to phone width.

## Developing

Run a daemon with a listener, then the Vite dev server, which proxies `/api`,
`/orgs/<org>/api` (websockets included) and `/healthz` to it and reloads on
every edit. Build and run both inside a sandbox ([Developing
isb](index.md#build-and-test-in-a-sandbox)); the daemon needs the incus
socket, so run the binary you built there on the host:

```sh
ISB_PUBLIC_URL=http://localhost:5173 isb serve --listen 127.0.0.1:8092 ...
cd web && ISB_URL=http://127.0.0.1:8092 bun run dev    # http://localhost:5173
```

`ISB_URL` defaults to `http://127.0.0.1:8092`. `ISB_PUBLIC_URL` must be the
page's origin for passkeys (the relying party checks it) and for provider
callbacks. Session cookies are not `Secure` over plain loopback HTTP, so
`http://localhost` works.

To refresh the typed client after adding or changing tools:

```sh
curl -s http://127.0.0.1:8092/api/v1/openapi.json | jq . > web/openapi.json
cd web && bun run gen:api
```

Add shadcn components with `bunx shadcn@latest add NAME` in `web/`
(`components.json` is set up), then check what it changed: imports should use
`@/lib/utils`, and no new runtime dependency should appear without a reason.
