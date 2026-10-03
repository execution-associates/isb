# Identity: users, roles, sessions and tokens

`isb serve` keeps its own users. They sign in with an email and a password,
a provider (GitHub, Google, any OpenID Connect provider) or a passkey, get a
browser session, join orgs by invitation, and make API tokens for scripts and
agents. Everything lives in one SQLite file, `<state>/isb.db`
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

## Signing in with GitHub, Google or OIDC

Providers are configured in `~/.config/isb/serve.env` (or flags), and need
`ISB_PUBLIC_URL`: their callback URL is built from it, and without it they
stay off.

```dotenv
ISB_PUBLIC_URL=https://isb.example.com

ISB_GITHUB_CLIENT_ID=Ov23li...
ISB_GITHUB_CLIENT_SECRET=...        # or a secret, see below

ISB_GOOGLE_CLIENT_ID=1234-abc.apps.googleusercontent.com
ISB_GOOGLE_CLIENT_SECRET=...

ISB_OIDC_ISSUER=https://idp.example.com/realms/main
ISB_OIDC_CLIENT_ID=isb
ISB_OIDC_CLIENT_SECRET=...
ISB_OIDC_NAME=Keycloak              # the button's label (default "SSO")

ISB_OPEN_SIGNUP=false               # see "Who may sign in"
```

The client ids, issuer and name also have flags (`--github-client-id`,
`--google-client-id`, `--oidc-issuer`, `--oidc-client-id`, `--oidc-name`,
`--open-signup`). **Client secrets are never flags** (argv shows in `ps`).
When `ISB_*_CLIENT_SECRET` is unset, isb reads a secret of the same name from
the `default` org's secrets store, at each sign-in (so rotating it needs no
restart):

```sh
isb secret create ISB_GITHUB_CLIENT_SECRET    # value on stdin; org default
```

At startup the daemon logs each provider's callback URL, or why a provider is
off (no secret, no public URL, an http issuer).

| Provider | Callback URL to register | Scopes |
|---|---|---|
| GitHub | `<public-url>/api/v1/auth/oauth/github/callback` | `read:user user:email` |
| Google | `<public-url>/api/v1/auth/oauth/google/callback` | `openid email profile` |
| OIDC | `<public-url>/api/v1/auth/oauth/oidc/callback` | `openid email profile` |

- **GitHub**: Settings, Developer settings, OAuth Apps, New OAuth App.
  Homepage URL: the public URL. Authorization callback URL: the row above
  (GitHub allows exactly one). Generate a client secret. isb reads `/user`
  and `/user/emails` and uses **only the primary address, and only if GitHub
  has verified it**. A GitHub App's OAuth credentials work the same way (give
  it the *Email addresses: read* account permission).
- **Google**: Google Cloud console, APIs & Services: configure the OAuth
  consent screen (External or Internal, scopes `openid`, `email`, `profile`;
  while it is in *Testing*, add the test users), then Credentials, Create
  credentials, OAuth client ID, type *Web application*, with the callback
  above under **Authorized redirect URIs** (no JavaScript origins needed).
- **Generic OIDC** (Keycloak, Authentik, Okta, Entra ID, Zitadel, Dex...): a
  confidential client using the authorization code flow, with the callback
  above as its redirect URI. `ISB_OIDC_ISSUER` is the issuer exactly as the
  provider's `/.well-known/openid-configuration` states it. The provider must
  put `email` and `email_verified` in the ID token or answer them at userinfo.

The issuer and every endpoint must be https (http is accepted only on
loopback, for local providers and tests).

### The flow

1. The login page sends the browser to `GET
   /api/v1/auth/oauth/PROVIDER/start?next=/path` (or `POST` the same path
   with `{next, invite, intent}` and follow the returned `url`, which keeps
   an invitation token out of URLs). isb makes a random `state`, a nonce, a
   PKCE verifier (S256), and a random binding value it sets in the cookie
   `isb_oauth` (HttpOnly, SameSite=Lax, `Path=/api/v1/auth/oauth/`, 10
   minutes, Secure as for sessions); it remembers the flow (in memory, single
   use, 10 minutes) and redirects to the provider.
2. The provider sends the browser back to `.../callback?code&state`. isb
   needs the state it issued **and** the binding cookie of the browser that
   started it, so a code from someone else's sign-in cannot be replayed into
   another browser. It trades the code (with the verifier) at the token
   endpoint:
   - OIDC (Google, generic): discovery and JWKS are fetched on first use and
     cached for an hour (an unknown key id refetches the JWKS, at most every
     10s). The ID token must be RS256 or ES256 from those keys, `iss` the
     discovered issuer, `aud` the client id (`azp` too when there are several
     audiences), `exp` not passed, `iat` not in the future (60s leeway), and
     `nonce` the one sent. Without an email in it, userinfo is asked.
   - GitHub: the access token reads `/user` and `/user/emails`.
3. The account rules below pick the user; isb starts a session, sets
   `isb_session`, and redirects (303) to `next`.

`next` must be a path on this site: one leading `/`, not `//`, no
backslash, no whitespace or control characters; anything else is refused
(`invalid_request`). Default `/`.

A failed browser flow redirects to `/login?error=CODE` (with `&next=` when
one was given; for linking, back to `next` with `?error=CODE`), and the
response body says the same as JSON. Codes: `unverified_email`,
`signup_closed`, `invitation_mismatch`, `invalid_invitation`,
`account_disabled`, `setup_required`, `identity_taken`, `state_invalid` (an
unknown, used or expired flow), `state_mismatch` (another browser's, or
another provider's), `provider_denied` (the user said no), `provider_error`
(the token, ID token or user lookup failed; details in the journal),
`provider_unavailable`, `unknown_provider`, `invalid_request`, `forbidden`,
`rate_limited`, `internal`. Start and callback count against the per-IP
limit.

### Who may sign in

In order:

1. An identity already linked (same provider and subject) signs its user in,
   whatever email it reports now.
2. Otherwise, a **verified** email that matches a user links the identity to
   that user and signs them in.
3. Otherwise, with a verified email, an account is created (no password,
   name from the provider) only if the email has a **pending invitation**,
   or `ISB_OPEN_SIGNUP=true`. Every pending invitation for the address is
   accepted. An invitation token passed to `start` (`invite`) must be for
   that same address. As in Dokploy, after the first admin, accounts come by
   invitation unless open sign-up is on.

An unverified email never links and never creates an account. Disabled users
are refused. Before first-run setup, nobody signs up through a provider.

Identities are recorded per provider (`github`, `google`, and `oidc:<issuer>`
for the generic one, so pointing it at another issuer cannot match old
subjects).

### Linking and unlinking

A signed-in user adds a provider with `start?intent=link&next=/settings`: the
callback links the identity to them (refused with `identity_taken` if it
belongs to someone else) and redirects without a new session. They list and
remove identities with `GET` and `DELETE identities`. **Nobody removes their
last way in**: a password, an identity and a passkey each count, and the
last one left is refused (`409`). Changing ways in needs a browser session,
not an API token.

## Passkeys

WebAuthn, verified by isb itself (CBOR and COSE parsed in-tree, signatures by
`ring`): ES256, EdDSA (Ed25519) and RS256 keys. The relying party is the host
of `ISB_PUBLIC_URL` and the origin its scheme, host and port; it must be
https or `http://localhost`, and a domain name, not an IP. Without it,
passkeys are off.

- Registration asks for a discoverable credential (`residentKey:
  required`) with user verification required, attestation `none`;
  attestation statements are not verified (a passkey is trusted because the
  signed-in user added it). The WebAuthn user id is a random 16-byte handle,
  the same for all of a user's passkeys.
- Every ceremony checks `clientDataJSON` (type, challenge, exact origin, no
  cross-origin), `rpIdHash`, user present and user verified. Sign-in also
  checks the signature with the stored key, the user handle when the browser
  sends one, and the signature counter: it must grow, unless the
  authenticator keeps none (both zero); a regression is refused as a likely
  clone. Concurrent sign-ins with one passkey cannot both pass.
- Challenges are 32 random bytes, single use, valid 5 minutes, held in
  memory (a restart forgets them).
- Sign-in is usernameless (the browser offers the site's passkeys); given an
  email, the options also list that user's passkeys in `allowCredentials`,
  which tells the caller whether that address has passkeys here.

The options are in the JSON form of `PublicKeyCredential.parseCreationOptionsFromJSON()`
and `parseRequestOptionsFromJSON()`; the browser's `credential.toJSON()` is
what the verify endpoints take (`clientDataJSON`, `attestationObject`,
`authenticatorData`, `signature` and `userHandle` base64url).

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
(429), `passkey_rejected` (401), `internal` (500), and the sign-in codes above
(403) for a refused provider sign-in. "Signed in" means a session cookie or a bearer token.

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
| `GET providers` | anyone | | `{providers: [{id, label, kind, start}], password: true, passkeys: bool, open_signup: bool}`; `kind` is `oauth2` or `oidc` |
| `GET oauth/PROVIDER/start` | anyone | query `next`, `invite`, `intent=login\|link` | `303` to the provider, `isb_oauth` cookie set |
| `POST oauth/PROVIDER/start` | anyone | `{next?, invite?, intent?}` | `{url}`, `isb_oauth` cookie set |
| `GET oauth/PROVIDER/callback` | the provider's redirect | query `code`, `state` | `303` to `next`, session cookie set; or `303` to `/login?error=CODE` |
| `GET identities` | signed in | | `{identities: [{id, user_id, provider, provider_id, label, subject, email, email_verified, created_at, last_used}]}` |
| `DELETE identities/ID` | signed in with a session | | `204`; `409` for the last way in |
| `POST passkeys/register/options` | signed in with a session | | `{publicKey: {rp, user, challenge, pubKeyCredParams, timeout, attestation, authenticatorSelection, excludeCredentials}}` |
| `POST passkeys/register/verify` | signed in with a session | `{name?, credential: {id, rawId, type, response: {clientDataJSON, attestationObject, transports?}}}` | `201 {passkey}` |
| `POST passkeys/login/options` | anyone | `{email?}` (or no body) | `{publicKey: {challenge, rpId, timeout, userVerification, allowCredentials}}` |
| `POST passkeys/login/verify` | anyone | `{credential: {id, rawId, type, response: {clientDataJSON, authenticatorData, signature, userHandle?}}}` | session, cookie set |
| `GET passkeys` | signed in | | `{passkeys: [passkey]}` |
| `DELETE passkeys/ID` | signed in with a session | | `204`; `409` for the last way in |

A session answer is `{user, memberships, session: {id, expires_at,
idle_expires_at}}`; the session token itself is only in the cookie. A user is
`{id, email, name, platform_admin, created_at, disabled, has_password}`. Times
are unix seconds. Someone outside an org gets `404` for its `orgs/ORG/...`
paths. A passkey is `{id, user_id, credential_id, name, alg, sign_count,
transports, aaguid, created_at, last_used}` (`credential_id` base64url,
`aaguid` hex).

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
sign-in: provider, subject, email, email_verified), `passkeys` (credential id,
user, user handle, COSE public key, algorithm, sign count, transports,
AAGUID, name, created_at, last_used), `orgs`, `memberships`, `sessions`,
`invitations`, `api_tokens`, `password_resets`, and `schema_version`. Migrations run at open, each in its own transaction; a
database from a newer isb is refused rather than downgraded.
