---
title: Sign-in and accounts
nav_title: Sign-in
description: Create the first admin, invite people, and let them sign in with a password, a passkey, GitHub, Google or your own OIDC provider.
order: 13
---

`isb serve` keeps its own users, so a platform needs nothing else to sign in
to it. People sign in with an email and a password, a passkey, or a provider
(GitHub, Google, any OpenID Connect provider), get a browser session, and join
orgs by invitation. Agents and scripts use API tokens instead ([Agents and
MCP](agents.md)). This page is the operator's and user's view; the protocol
details (the OAuth flow, passkey verification, sessions, CSRF, rate limits,
every endpoint) are in the [Identity API](../reference/identity-api.md), and
roles and tokens in [Users, roles and superadmins](../concepts/access.md).

Everything lives in one SQLite file, `<state>/isb.db` (`--state-dir`, default
`$XDG_STATE_HOME/isb`), mode 0600 in a 0700 directory.

## The first admin

While no user exists, make one of two ways:

- **On the host**, as the daemon's user:

  ```sh
  isb user create you@example.com      # prompts for the password twice
  ```

  It reads stdin's first line instead when stdin is not a terminal. The first
  user is always a platform admin and owner of the `default` org.
- **Through the web**: at startup the daemon writes a one-time **setup token**
  to `<state>/setup-token` (0600) and logs where it is. Open `/setup` (the
  link `/setup#TOKEN` carries it in the fragment), or `POST
  /api/v1/auth/setup` with it, so whoever reaches the port first cannot claim
  the platform. The file is removed once setup is done.

## Passwords

- At least 12 characters (at most 1024 bytes).
- Hashed with argon2id (19 MiB, 2 passes, 1 lane: OWASP's recommendation).
- A failed login always answers `401 invalid email or password`, whether the
  email is unknown, the password wrong, the account disabled or without a
  password, and costs one argon2 verification either way, so neither the
  message nor the timing says which half was wrong.
- Changing a password (the Account page) needs the current one and ends every
  other session. `isb user passwd EMAIL` sets one from the host and ends all of
  them.
- Passwords never come from argv, where they would show in `ps` and shell
  history.

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
the `default` org's secret store, at each sign-in (so rotating it needs no
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
loopback, for local providers and tests). The flow uses PKCE, a nonce and a
browser-bound state cookie; see [the flow](../reference/identity-api.md#the-flow).

A failed sign-in lands on `/login?error=CODE`, and the login page explains
the code in plain words (`unverified_email`, `signup_closed`,
`invitation_mismatch`, `account_disabled`, ...; the full list is in the
Identity API).

### Who may sign in

In order:

1. An identity already linked (same provider and subject) signs its user in,
   whatever email it reports now.
2. Otherwise, a **verified** email that matches a user links the identity to
   that user and signs them in.
3. Otherwise, with a verified email, an account is created (no password,
   name from the provider) only if the email has a **pending invitation**,
   or `ISB_OPEN_SIGNUP=true`. Every pending invitation for the address is
   accepted. An invitation token passed to the sign-in must be for that same
   address. After the first admin, accounts come by invitation unless open
   sign-up is on, as in Dokploy.

An unverified email never links and never creates an account. Disabled users
are refused. Before first-run setup, nobody signs up through a provider.

Identities are recorded per provider (`github`, `google`, and `oidc:<issuer>`
for the generic one, so pointing it at another issuer cannot match old
subjects).

### Linking and unlinking

A signed-in user adds a provider from the Account page: the identity is linked
to them (refused with `identity_taken` if it belongs to someone else) without
a new session, and removed there too. **Nobody removes their last way in**: a
password, an identity and a passkey each count, and the last one left is
refused. Changing ways in needs a browser session, not an API token.

## Passkeys

Passkeys work once `ISB_PUBLIC_URL` is set: the relying party is its host and
the origin its scheme, host and port. It must be https or
`http://localhost`, and a domain name, not an IP; without it, passkeys are
off.

- Add one on the Account page while signed in; it asks the authenticator for
  a discoverable credential with user verification. ES256, EdDSA (Ed25519)
  and RS256 keys are accepted.
- Sign-in is usernameless: the login page's passkey button lets the browser
  offer the site's passkeys.
- A passkey whose signature counter goes backwards is refused as a likely
  clone.

isb verifies WebAuthn itself; the checks are listed under
[Passkeys](../reference/identity-api.md#passkeys) in the Identity API.

## Sessions

Signing in sets the `isb_session` cookie (HttpOnly, SameSite=Lax, Secure
except over plain loopback HTTP, so `http://localhost` works in development).
A session ends 30 days after sign-in (`--session-max-age`) or after 7 days
unused (`--session-idle`), whichever comes first. Users list their sessions
on the Account page and end any of them; disabling a user ends theirs.

## Invitations

An org's owner or admin (or a platform admin) invites an email to the org
with a role, from the web UI's **Members** page or the host CLI:

```sh
isb invite acme alice@example.com --role member    # viewer, member, admin or owner
```

The invitation is valid for 7 days and single use; inviting the same address
to the same org again replaces it. It is shown once: the token, and a link
`<public-url>/invite#<token>` when `--public-url` is set (the token sits in the
fragment, which browsers never send to a server).

Accepting, at `/invite#TOKEN`:

- a new address: give a name and a password; the account is created and
  signed in (or sign in with a provider whose verified email is that address);
- an existing account: give its password (the invitation alone never signs
  anyone in as someone else), or accept while signed in as that address;
- accepting never lowers a role the user already has in the org.

## Password resets

`/forgot-password` (or `POST password-reset/request`) always answers the
same, whether or not the account exists. For an existing account it makes a
one-hour, single-use token (only the newest works) and delivers it. **isb has
no mailer, so the daemon writes the reset link (or token) to its stderr,
that is its journal, and says so**; an operator hands it over:

```sh
journalctl --user -u isb | grep -i reset
```

Opening `/reset-password#TOKEN` sets the new password and ends every session
of the user. An operator on the host can skip the link: `isb user passwd
EMAIL`.

## Managing users

Platform admins manage users from the web UI's **Platform** page (or the
`admin/users` endpoints): disable and enable them, and make or unmake
platform admins. Nobody does either to themselves, and an enabled platform
admin always remains.

On the host, as the daemon's user, these commands open `<state>/isb.db`
directly, so they work before any user exists and while the daemon is down.
SQLite in WAL mode lets the daemon and the CLI share the file, and the daemon
reads sessions and tokens per request, so changes apply at once:

```text
isb user create EMAIL [--admin] [--name N]   password prompted twice, or stdin's first line
isb user ls [--json]                          users, flags, org memberships
isb user passwd EMAIL                         set a password, end their sessions
isb invite ORG EMAIL [--role member]          prints the token, or the link with ISB_PUBLIC_URL
isb token create NAME [--org ORG] [--expires 90d] [--user EMAIL] [--scope S]...
isb token ls [--json]
isb token revoke ID...
```

Every change these commands make is recorded in the [audit
log](../operations/audit.md) as `local(uid N)` on the `cli` surface, and so is
every sign-in, account, token, invitation, member and user change made over
HTTP.

## With Cloudflare Access in front

With Access configured, it stays the front door: the sign-in pages and
`/api/v1/auth/*` also need a valid Access assertion, and isb's own sign-in
applies behind it. An Access identity whose email is an isb user acts as that
user without signing in again ([Reach isb serve remotely](remote-access.md)).
