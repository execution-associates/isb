# Identity: users, roles, sessions and tokens

`isb serve` keeps its own users. They sign in with an email and a password,
get a browser session, join orgs by invitation, and make API tokens for
scripts and agents. Everything lives in one SQLite file, `<state>/isb.db`
(`--state-dir`, default `$XDG_STATE_HOME/isb`), mode 0600 in a 0700
directory. The endpoints are served on the daemon's loopback HTTP listener
(`--listen`) under `/api/v1/auth/`, next to `/mcp` and `/healthz`.

## Orgs and roles

An org is the trust boundary: its members administer what is in it (apps,
stacks, sandboxes, secrets), and nothing crosses orgs. Each membership has a
role:

| Role | Administer the org's apps and secrets | Manage members, invitations, every token in the org | Delete the org |
|---|---|---|---|
| `member` | yes | | |
| `admin` | yes | yes | |
| `owner` | yes | yes | yes |

A **platform admin** (a flag on the user) can do everything in every org.

- An actor grants roles up to its own: an admin can add members and admins,
  only an owner (or platform admin) can make or change an owner.
- An org always keeps one owner: the last one cannot be demoted or removed.
- Anyone can leave an org.
- Roles are stored as text and mapped to permissions in one table in code
  (`Role::permissions`), so a new role or a finer permission needs no migration.

## The first admin

While no user exists, either:

- on the host, as the daemon's user: `isb user create you@example.com`
  (prompts for the password, or reads stdin's first line when not a terminal).
  The first user is always a platform admin and owner of the `default` org.
- or through the web: at startup the daemon writes a one-time **setup token**
  to `<state>/setup-token` (0600) and logs where it is. `POST
  /api/v1/auth/setup` needs it, so whoever reaches the port first cannot claim
  the platform. The file is removed once setup is done.

## Passwords

- At least 12 characters (at most 1024 bytes).
- Hashed with argon2id (19 MiB, 2 passes, 1 lane: OWASP's recommendation),
  stored as a standard PHC string.
- A failed login always answers `401 invalid email or password`, whether the
  email is unknown, the password wrong, the account disabled or without a
  password, and costs one argon2 verification either way, so neither the
  message nor the timing says which half was wrong.
- Changing a password (`POST password`) needs the current one and ends every
  other session. `isb user passwd EMAIL` sets one from the host and ends all of
  them.

## Sessions

Signing in sets the cookie `isb_session`:

- `HttpOnly; SameSite=Lax; Path=/`, and `Secure` unless the request came over
  plain loopback HTTP (a loopback peer, a loopback `Host`, and no
  `X-Forwarded-Proto: https` or `Cf-Visitor` https), so `http://localhost`
  works in development and anything through the tunnel gets a Secure cookie.
- A session ends 30 days after sign-in (`--session-max-age`) or after 7 days
  unused (`--session-idle`), whichever comes first. Each use slides the idle
  limit.
- Sign-out deletes it. Users can list their sessions and end any of them.
  Disabling a user ends theirs.

## API tokens

`Authorization: Bearer isb_tok_...`. A token belongs to a user and is either:

- **org-scoped**: confined to one org, with the user's current role in it
  (read on every request, so a demotion or removal applies at once; leaving
  the org deletes the user's tokens for it). Any member can make one for
  their own org. A platform admin's org token acts as owner of that org and is
  not a platform admin.
- **platform** (no org): the user's whole reach. Platform admins only; it stops
  working if the user stops being one.

Tokens can expire (`--expires 90d`, `"expires": "90d"`) or not. Each records
when it was last used. Users list and revoke their own; an org's owners and
admins list and revoke any token in it. An org token cannot mint a token for
another org or a platform token.

A request carrying `Authorization` is judged by it alone: a bad token is a
401 even with a valid session cookie.

## Secrets at rest

Every bearer secret (session `isb_sess_`, API token `isb_tok_`, invitation
`isb_inv_`, password reset `isb_rst_`, setup token `isb_setup_`) is 32 random
bytes from the OS, base64url behind its prefix, shown exactly once. The
database keeps only the SHA-256 of each; the row found by that hash is
compared again in constant time before it is trusted.

## Invitations

An owner or admin (or platform admin) invites an email to an org with a role.
The invitation is valid for 7 days and single use; inviting the same address
to the same org again replaces it. The response carries the token, and a link
`<public-url>/invite#<token>` when `--public-url` is set (the token sits in the
fragment, which browsers never send to a server).

Accepting:

- a new address: give a name and a password; the account is created and
  signed in.
- an existing account: give its password (the invitation alone never signs
  anyone in as someone else), or accept while signed in as that address.
- accepting never lowers a role the user already has in the org.

## Password resets

`POST password-reset/request` always answers `202`, whether or not the account
exists. For an existing account it makes a one-hour, single-use token (only
the newest works) and delivers it. **No mailer is configured yet, so the daemon
writes the reset link (or token) to its stderr, i.e. its journal, and says
so**; an operator hands it over. Confirming sets the password and ends every
session of the user.

## CSRF

Every request other than `GET`/`HEAD` under `/api/v1/auth/` must carry the
header `X-Isb-Csrf: 1`, unless it carries `Authorization: Bearer`. Browsers
send a custom header cross-origin only after a CORS preflight, which isb never
grants, so a forged form post or fetch from another site is refused (`403
csrf`) before it does anything. Login and setup are covered too. The web UI
sends the header on every request.

## Rate limits

In memory, per daemon (they reset on restart), as token buckets:

| What | Key | Burst, then |
|---|---|---|
| sign-in | email | 5, then 1 per minute |
| sign-in, setup, invitation inspect/accept, reset request/confirm | client IP | 20, then 1 per 6 s |
| reset requests | email | 3, then 1 per 15 minutes |

Over the limit is `429` with `Retry-After`. The client IP is
`Cf-Connecting-IP` when the peer is loopback (the tunnel), else the peer.

## Cloudflare Access

With Access configured (`CF_ACCESS_TEAM_DOMAIN`, `CF_ACCESS_AUD`), it stays the
front door: `/api/v1/auth/*` also needs a valid `Cf-Access-Jwt-Assertion`, as
`/mcp` does, and then isb's own sign-in applies behind it. Without Access the
identity endpoints are still served (they authenticate their own callers);
remote MCP stays off unless explicitly enabled.

## Endpoints

JSON in and out; every response is `Cache-Control: no-store`. Errors are
`{"error": CODE, "message": TEXT}` with `CODE` one of `invalid_credentials`
(401), `unauthenticated` (401), `invalid_token` (400), `invalid` (400), `csrf`
(403), `forbidden` (403), `not_found` (404), `conflict` (409), `rate_limited`
(429), `internal` (500). "Signed in" means a session cookie or a bearer token.

| Method and path (`/api/v1/auth/...`) | Who | Body | Answer |
|---|---|---|---|
| `GET setup` | anyone | | `{"needed": bool}` |
| `POST setup` | anyone, with the setup token | `{setup_token, email, name, password}` | `201` session (below), cookie set |
| `POST login` | anyone | `{email, password}` | session, cookie set |
| `POST logout` | anyone | | `204`, cookie cleared |
| `GET me` | signed in | | `{user, platform_admin, memberships: [{org, role}], auth: {kind: "session", id} \| {kind: "api_token", id, org}}` |
| `GET sessions` | signed in | | `{sessions: [{id, created_at, last_seen, expires_at, idle_expires_at, user_agent, ip, current}]}` |
| `DELETE sessions/ID` | signed in | | `204` |
| `POST invitations` | org owner/admin | `{org, email, role?}` (default member) | `201 {invitation, token, link}` |
| `POST invitations/inspect` | anyone with the token | `{token}` | `{org, email, role, expires_at, account_exists}` |
| `POST invitations/accept` | anyone with the token | `{token, name?, password?}` | `{user, membership, created}`, cookie set unless already signed in |
| `GET tokens` | signed in | | `{tokens: [{id, name, user_id, org, created_at, last_used, expires_at}]}` |
| `POST tokens` | signed in | `{name, org?, expires?}` | `201 {token, info}` |
| `DELETE tokens/ID` | its user, or the org's owners/admins | | `204` |
| `POST password` | signed in with a session | `{current_password, new_password}` | `204` |
| `POST password-reset/request` | anyone | `{email}` | `202 {"ok": true}` |
| `POST password-reset/confirm` | anyone with the token | `{token, password}` | `204` |
| `GET orgs/ORG/members` | org members | | `{members: [{user, role}]}` |
| `PUT orgs/ORG/members/USER_ID` | org owner/admin | `{role}` | `{user_id, role}` |
| `DELETE orgs/ORG/members/USER_ID` | org owner/admin, or the member leaving | | `204` |
| `GET orgs/ORG/invitations` | org owner/admin | | `{invitations: [...]}` (pending) |
| `DELETE orgs/ORG/invitations/ID` | org owner/admin | | `204` |
| `GET orgs/ORG/tokens` | org owner/admin | | `{tokens: [...]}` |

A session answer is `{user, memberships, session: {id, expires_at,
idle_expires_at}}`; the session token itself is only in the cookie. A user is
`{id, email, name, platform_admin, created_at, disabled, has_password}`. Times
are unix seconds. Someone outside an org gets `404` for its `orgs/ORG/...`
paths.

## CLI

These open `<state>/isb.db` directly (as the daemon's user; `--state-dir` or
`ISB_SERVE_STATE_DIR`), so they work before any user exists and while the
daemon is down. SQLite in WAL mode lets the daemon and the CLI share the file,
and the daemon reads sessions and tokens per request, so changes apply at once.

```text
isb user create EMAIL [--admin] [--name N]   password prompted twice, or stdin's first line
isb user ls [--json]                          users, flags, org memberships
isb user passwd EMAIL                         set a password, end their sessions
isb invite ORG EMAIL [--role member]          prints the token, or the link with ISB_PUBLIC_URL
isb token create NAME [--org ORG] [--expires 90d] [--user EMAIL]
                                              prints the token once; --user defaults to the only platform admin
isb token ls [--json]                         metadata only
isb token revoke ID...
```

Passwords never come from argv, where they would show in `ps` and shell
history.

## Schema

Tables: `users` (email unique, case-insensitive), `user_identities` (external
sign-in: provider, subject, email, email_verified), `orgs`, `memberships`,
`sessions`, `invitations`, `api_tokens`, `password_resets`, and
`schema_version`. Migrations run at open, each in its own transaction; a
database from a newer isb is refused rather than downgraded.
