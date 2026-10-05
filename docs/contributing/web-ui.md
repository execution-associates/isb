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
- `GET /api/v1/events` (server-sent events) for live updates, over one
  connection the whole app shares, reconnecting with backoff (1 s doubling to
  30 s) and resuming after the last event seen
  ([Keeping data live](#keeping-data-live)).
- `GET /orgs/<org>/api/v1/terminal` (a websocket) for terminals
  ([HTTP API](../reference/http-api.md#the-web-terminal)).

The UI hides what a role may not do; the server decides, and its refusals are
shown as it words them.

## Keeping data live

A page never needs a reload to show what the server has now, whoever changed
it: this browser, another session, the CLI or an agent. Server state lives
in the react-query cache, never copied into component state or context
(only log followers and revealed credentials are held outside it), and four
rules keep that cache current. `web/src/lib/freshness.ts`
holds the defaults and the event mapping, with tests beside it.

- **Nothing is trusted from cache.** The `QueryClient` defaults make every
  query stale at once (`staleTime: 0`) and refetch it when it mounts, when
  the window regains focus and when the network comes back. A query sets a
  longer `staleTime` only for data that changes with a release (the tool
  list, providers, templates).
- **Events invalidate.** `useLiveSync`, mounted once by the app shell,
  follows `GET /api/v1/events` for every org the user sees. An event about
  `org/stack` (a bare name is the default org's) invalidates `["apps", org]`,
  `["stacks", org]` and `["tool", "stack_list"]`, gathered for 300 ms so a
  rollout's burst costs one refetch of each active query; deployment log
  lines (level `log`) invalidate nothing. Pages do not subscribe for
  freshness themselves; they use `useLiveEvents` only to react to an event
  (a toast, a log line, following a deployment).
- **A reconnect resyncs.** Events may be missed while the stream is down, so
  when it reopens every active query refetches. The daemon numbers events
  from 1 each time it starts and answers a cursor from before a restart from
  the start, so the client keeps the last `seq` it received, not the
  highest.
- **What no event announces is polled.** Events cover stacks (deploys,
  rollouts, health, restarts, removals). A project, environment or app made,
  changed or removed elsewhere emits none, so the live views poll every
  `LIVE_POLL` (15 s) while the tab is visible: the project, app and stack
  lists, a stack's status and the org overview. Pages with their own pace
  (a deployment in progress, logs, metrics, workspaces) set a
  `refetchInterval` of their own. The signed-in user (`["me"]`, whose orgs
  fill the org switcher) refetches every minute.

Every key for org-scoped data carries the org right after its family
(`["apps", org, ...]`, `["stacks", org, ...]`, `["workspace", org]`,
`["templates", "list", org]`), so switching orgs never shows another org's
cached data, and invalidating one org leaves the others alone. One tool
answer has one key: `secret_list` is `["apps", org, "secret-list"]` and
`org_get` is `["tool", "org_get", org]` wherever they are shown. Put a new
org-scoped query under `["apps", org]`, where events already reach it.

A mutation in an org calls `invalidateOrg(qc, org)`, which invalidates the
same prefixes an event does, rather than naming the one key on screen: the
list, the detail, the overview and the stack list all follow. Pages outside
those families invalidate their own keys and every copy of the data (an
org-bound token shows under `["tokens"]` and `["org-tokens", org]`; a server
operation refreshes all of `["tool"]`). Invalidating a prefix is cheap,
because only the queries on screen refetch.

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
  softly rounded 8 px corners (4 px on small controls), and the site's coast at night on the sign-in panel.
  The site is dark only, so the theme is dark only: `<html>` carries `dark`
  and `ea`, and `:root.ea` replaces the dark palette.
- **Texture** (Execution Associates theme only; `web/src/ea-texture.css`,
  `lib/texture.ts`, `lib/texture-gl.ts`, `components/texture-layer.tsx`): the
  site's still film grain and ordered-dither light, ported from its own
  `texture.js`. A fixed WebGL canvas sits *behind* the app and the shell is
  transparent, so grain and a dithered peach-to-violet glow show on the ground
  (sidebar, page, behind page headers) while every opaque surface (cards,
  tables, dialogs, menus, the terminal and logs) stays a clean plate; nothing
  dense has texture under it. The canvas draws once, then only on scroll and
  resize (whole-pixel scroll so the grain never shimmers), never on a timer
  and not while the tab is hidden. Without WebGL, or on a software rasteriser
  (SwiftShader, llvmpipe), `selectRenderer` picks CSS grain (a tiled SVG
  noise on the body) instead. `<html>` carries `data-texture-fx` (`gl`, `css`,
  `none`), set to `css` before paint by `web/public/theme.js`. Shorter dithered
  fades that CSS can do without a canvas are Bayer-threshold SVG masks in
  2px cells (`web/src/ea-ramps.css`, generated by
  `bun web/scripts/dither-ramps.ts`): meter fills trail off in dither, the
  deployment strip's connectors, a dialog's and card's top edge, empty states'
  floor, the sign-in panel's edge, and the initials and org badges. Smaller
  signatures: peach selection, a line of light under page titles, a lit bar
  on the current sidebar page, and a glow on the primary button. Everything
  is scoped to `html.ea` (a test enforces it). Light-on-ink contrast is
  measured at the glow's brightest point: muted text stays above 5:1.
  Component surfaces carry subtle gradients too (`web/src/ea-gradients.css`):
  lit from above with a peach wash at one corner, painted as `background-image`
  over the token colour on cards, dialogs, sheets, menus and the palette; a
  top-lit sheen on buttons; a faint recess on inputs and selects; the table
  header row, the sidebar's current page and page headers (a faint wash,
  as they sit on the grain). `--ea-gk` scales every layer, and `--ea-surface-gradient`, `--ea-sheen`
  and `--ea-field-gradient` are the recipes to tune. Plates carry a faint
  noise so a large gradient does not band. Muted text stays above 5.5:1 at the
  lightest point of any plate.
  Everything is bundled: the CSP's `img-src 'self' data:` covers the SVG
  masks and noise, and no script is inline. To try the WebGL path in a
  headless browser (software rendering falls back to CSS), patch
  `WebGLRenderingContext.prototype.getParameter` to hide the renderer name,
  then toggle the texture setting.
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
