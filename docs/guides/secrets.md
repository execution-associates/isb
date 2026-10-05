---
title: Secrets
description: Keep passwords, tokens and keys in an org's encrypted store, and deliver them to services as files or variables.
order: 10
---

Every org on `isb serve` has its own secret store: named values, encrypted at
rest, that you manage with `isb secret` or the `secret_*` tools. Stacks and
apps refer to stored secrets by name and version, never by value, so a value
never sits in a compose file, an instance's config or a deployment record,
and giving a secret a new version cycles the services that use it, the way
each one asks ([When a secret changes](#when-a-secret-changes)).

```sh
printf %s "$DB_PASSWORD" | isb secret create db_password    # value from stdin
isb secret set db_password ./new-password.txt               # or from a file: version 2
isb secret ls
isb secret inspect db_password                              # metadata, never the value
isb secret get db_password                                  # the value, raw, to stdout
isb secret rm db_password
```

Every command talks to the daemon over its unix socket, as `isb stack` does,
and takes `--org ORG` (default: the `default` org).

## The store

A secret is a name in an org, with a value and metadata: the driver that holds
it, a version, created and updated times (unix seconds), and labels.

- **Names** are 1-128 characters of `[A-Za-z0-9_.-]`, not starting with `.`.
  Names are unique within an org across all drivers.
- **Versions** start at 1, and every `set` adds one. Only the current value is
  kept; there is no history.
- **Values** are at most 1 MiB, and come from a file or stdin, never from a
  command-line argument (which other users can read in `/proc` and which
  lands in shell history).
- **Listing never shows values.** `ls` and `inspect` (and `secret_list`,
  `secret_inspect`) return metadata only; `get` is the only way to read one.

Inside an org, every member (not viewers) may read every secret value: the
org is the trust boundary ([Users, roles and
superadmins](../concepts/access.md#roles)). The web UI keeps values off
screen unless an owner or admin presses **Reveal**.

## Drivers

Each secret lives in one driver, chosen at `create` (`--driver`, default
`local`); later calls find it by name. A driver may be read-only, in which
case `create`, `set` and `rm` are refused.

### `local`

The value is age-encrypted on the daemon's disk:

```text
<state-dir>/orgs/<org>/secrets/<name>.age    the value, age ciphertext (binary)
<state-dir>/orgs/<org>/secrets/<name>.json   its metadata
```

The state directory is `$XDG_STATE_HOME/isb` (or `--state-dir`). Files are
0600 in 0700 directories, and every write goes to a temp file, is fsynced and
renamed into place, so a crash never leaves half a value. A `.age` file is a
standard age file: `age -d -i KEY <name>.age` decrypts it with the daemon's
key or a break-glass key.

`isb secret refresh NAME` re-reads a secret from its source and, if its
version moved, cycles the services using it; for `local` it does nothing.

### `onepassword`

Values that live in 1Password, read with the 1Password CLI (`op`, from
`$ISB_OP_BIN` or `$PATH`) and the org's own service-account token. Read-only:
manage the values in 1Password.

- Give the org its token once, as a `local` secret named `onepassword-token`:
  `printf %s "$TOKEN" | isb --org acme secret create onepassword-token -`.
  Each org uses only its own token, and an org without one is refused.
- Refer to a value as `vault/item/field` (or `vault/item/section/field`): the
  `op://` path without its scheme. When an item's title contains a `/`, use
  its ID (`op item list --vault V --format json`).
  `isb --org acme secret get ops/smtp/password` reads one.
- A name with a `/` in it is always such a reference; `local` names never
  contain one.
- The version is the 1Password item's version, which moves on every edit to
  the item, so the daemon's refresh notices a rotation. Every field of an
  item shares it, so editing one field moves the version of every reference
  into that item, and the services using any of them cycle.
- Polling is grouped to stay inside 1Password's daily read limit (per
  account, across all its service accounts): a round asks `op item get`
  once per org, vault and item, however many fields, stacks and services
  refer to it, and the values a new version then needs come from that same
  answer (kept 20 seconds) rather than one read per reference and replica.
  A field the item's JSON does not carry as text (a file, an OTP) is read
  with `op read`. Budget about one read per item per `refresh` interval:
  50 items at `refresh: 5m` is 14,400 reads a day. Workspaces' references
  are polled in a round of their own, per org, every `secret_refresh`.
- `isb secret refresh REF` asks 1Password once, however many stacks use it.
- The token reaches `op` through the environment of that one child process,
  never its arguments, and nothing else from the daemon's environment goes
  with it.

The web UI's Secrets page explains the same setup and lists the driver
references deployed stacks read.

## The daemon's key

Values are encrypted to the daemon's own age (X25519) identity. It is looked
up in this order, first hit wins:

1. `ISB_AGE_KEY`: the key itself (an `AGE-SECRET-KEY-1...` line, or a whole
   `age-keygen` file).
2. The systemd credential `$CREDENTIALS_DIRECTORY/isb-age-key`
   (`LoadCredential=` or `LoadCredentialEncrypted=` in the unit).
3. `ISB_AGE_KEY_FILE`: a key file. It must exist when set.
4. `~/.config/isb/age.txt` (`$XDG_CONFIG_HOME/isb/age.txt`).

If none is found, the daemon generates a new identity at
`~/.config/isb/age.txt` (0600, in a 0700 directory) in `age-keygen`'s format,
and logs one line saying where. **Keep that file out of unencrypted
backups**, and add a break-glass recipient: without one, losing the key loses
every secret. At startup the daemon logs where the key came from and its
public key, and warns when there is no break-glass recipient.

### As a systemd credential

`isb serve install` moves the key into an encrypted systemd credential where
it can: with systemd 256 or later (`systemd-creds --version`), it encrypts the
key (generating one if there is none) with `systemd-creds encrypt --user
--name=isb-age-key` into `~/.config/isb/isb-age-key.cred` (0600) and adds
`LoadCredentialEncrypted=isb-age-key:%h/.config/isb/isb-age-key.cred` to the
unit, so the daemon reads it from `$CREDENTIALS_DIRECTORY` (lookup step 2).
Running the installer again re-encrypts the plaintext key if there is one,
and otherwise keeps the credential; it never makes a new key while a
credential exists.

The plaintext `age.txt` is then unused by the daemon; removing it is your
call, and the installer prints the command (`shred -u
~/.config/isb/age.txt`). **Add a break-glass recipient first** (and run `isb
secret reencrypt --all`): the credential is bound to this machine and user,
so it cannot be decrypted anywhere else, and without the plaintext file or a
break-glass key, losing the host loses every secret. With the plaintext gone,
`isb up` reads store secrets only through a running daemon.

On a systemd older than 256 the daemon keeps reading `age.txt`, and the
installer says to keep it out of backups.

## Break-glass recipients

`~/.config/isb/secrets.toml` (or the file `$ISB_SECRETS_CONFIG` names) lists
recipients every value is also encrypted to, so it can be recovered without
the daemon:

```toml
recipients = [
  "age1...",                                    # an age public key
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops@example.com",  # or an SSH one
]
```

Each entry is an age X25519 public key (`age1...`) or an SSH public key
(`ssh-ed25519` or `ssh-rsa`, comment optional). A malformed entry stops the
daemon at startup. The file is read when the daemon starts; after changing
it, restart the daemon and re-encrypt what is already stored:

```sh
systemctl --user restart isb
isb secret reencrypt --all     # every org; or --org ORG
```

To recover a value with a break-glass key: `age -d -i ~/.ssh/id_ed25519
<state-dir>/orgs/<org>/secrets/<name>.age`.

## Inline values in compose files

`isb secret encrypt` encrypts a value for a compose file's `age:` field. With
no `--recipient`, it asks the daemon for its recipients (its own public key
and the break-glass ones) and encrypts locally, so the value never reaches
the daemon. With `--recipient` (repeatable), it needs no daemon at all.

```sh
printf %s "$API_TOKEN" | isb secret encrypt > token.age
isb secret encrypt -r age1... -r "ssh-ed25519 AAAA..." ./token.txt
```

The output is ASCII-armored age (`-----BEGIN AGE ENCRYPTED FILE-----`), the
format `age -a` writes, so it is safe to commit and readable with the age CLI.
In YAML, use a block scalar:

```yaml
secrets:
  api_token:
    age: |
      -----BEGIN AGE ENCRYPTED FILE-----
      YWdlLWVuY3J5cHRpb24ub3JnL3YxCi0+IFgyNTUxOSBC...
      -----END AGE ENCRYPTED FILE-----
```

`age:` also accepts base64 of age's binary format, on one line or several.

## Compose fields

A top-level secret has exactly one source:

| Field | Value comes from |
|---|---|
| `file` | A host file, read by whoever deploys the file. |
| `environment` | An environment variable of whoever deploys the file. |
| `external: true` | The org's secret store, under `name` (default: the key). |
| `age` | The inline ciphertext, decrypted with the daemon's key. |
| `driver` + `name` | A secrets driver, by that driver's reference; `refresh` (default `1h`, at least `10s`) is how often a stack checks it for a new version. |

A service uses a secret as a file (`secrets:`, under `/run/secrets`) or as an
environment variable (`environment: {KEY: {secret: NAME}}`):

```yaml
secrets:
  db_password: {external: true}
  tls_key: {external: true, name: web.tls-key}
  api_token: {age: "..."}
services:
  web:
    image: dev-base
    command: [./serve]
    secrets: [db_password, {source: tls_key, target: key.pem}]
    environment:
      API_TOKEN: {secret: api_token}
```

The full field reference (targets, owners, modes, how files survive a reboot)
is in the [isb.yaml reference](../reference/compose.md).

### As environment variables

`KEY: {secret: NAME}` delivers the value as the variable `KEY`. It must be
text (UTF-8, no NUL); mount anything else as a file.

- **System images:** the variable is written only to the supervised command's
  0600 environment file in the guest (`/etc/isb/<service>.env`, the unit's
  `EnvironmentFile=`), and given to a foreground `isb up` command through
  exec. It never reaches instance config. The service needs a `command`.
- **OCI images:** the app is the instance's init process, which only incus
  can give an environment, so the variable is instance config
  (`environment.KEY`). **That is plaintext in the incus database**, visible to
  anyone who can run `incus config show` on the instance (or read incusd's
  database, or a backup of it). isb shows it as `(secret)` in plans and
  reports. Prefer a file mount (`secrets:`) when the image can read one.

## Stacks

A deployed stack never holds a secret's value. For each top-level secret its
services use, the definition records a reference: the name in the org's
store (or a driver's reference), the driver, and the version deployed
(`stack_config` shows them). Where the value comes from:

- `external: true` names an existing secret in the stack's org; the deploy
  fails if there is none.
- `file:` and `environment:` are read by the client running `isb stack
  deploy`, which sends the values; the daemon stores each as a `local` secret
  named `<stack>_<key>` in the org, as swarm does. A value equal to the stored
  one keeps its version.
- `age:` is decrypted by the daemon and stored the same way.
- `driver: X, name: REF` is read through driver X.

The controller reads each value from the store whenever it delivers it (a new
instance, a reboot, a restart in place), never from the definition. A new
version is noticed in two ways:

- `isb secret set NAME` (or `secret_set`) moves every stack in the org bound
  to `NAME` to the new version right away.
- Driver-backed secrets are polled: every `refresh` interval (default 1h) the
  controller asks the driver for the current version. `isb secret refresh
  NAME` (a store name or a stack's driver reference) checks now.

What each service then does is its `on_change` ([When a secret
changes](#when-a-secret-changes)).

`isb stack rm` deletes the `<stack>_<key>` secrets the stack stored, unless
another stack has come to use them. `isb secret rm` refuses to delete a
secret that a deployed stack uses. The store keeps only each secret's current
value, so `isb stack rollback` delivers today's values.

Apps use the same mechanism: an app's env secret `NAME` (and a file's
secret) becomes the stack secret `<app>.NAME` (`external`, store name
`NAME`), so `isb secret set NAME` reaches the app, per its
`secret_on_change` ([Deploy apps](deploy-apps.md)).

## When a secret changes

Each stack service using a secret chooses what its new version does, with
[`on_change`](../reference/compose.md#on_change) on the top-level secret or
on the service's own reference (an app: `secret_on_change`):

| `on_change` | The replicas | Gap |
|---|---|---|
| `roll` (default) | Replaced by a rolling update per `update_config`: `start-first` starts each new replica and waits until it is healthy before retiring the old one; `failure_action` applies. | None with `start-first` |
| `restart` | Kept. One batch at a time (`update_config.parallelism`), each is drained, gets the new files and variables, has its app restarted, and must pass its health check before the next. | That replica, while it restarts |
| `none` | Kept, and not restarted. Files get the new value (an app that re-reads them picks it up); variables get it for the next start. Reported stale. | None |

Everything that uses a secret, and what a new version does to it:

| Consumer | On a new version |
|---|---|
| Stack service, file (`secrets:`) | Its `on_change`. |
| Stack service, variable (`environment: {KEY: {secret: NAME}}`) | Its `on_change`. A running process cannot take a new variable, so `none` delivers it for the next start only. |
| App (`${{secret.NAME}}` in env, `files`) | A stack service: its `secret_on_change` (default `roll`; apps update `start-first` unless they have volumes). |
| Database app | Its passwords (`db.<app>.password`, MySQL's and MariaDB's `db.<app>.root-password`) carry a `rotate` command: before anything is stored, isb changes the password inside the running database, authenticating with the old one, then stores the new value and rewrites `db.<app>.url` and the database's `urls` (the same URL with a driver's options), so apps using them follow. A copy of the password anywhere else goes stale: give it to apps through `urls` instead. If that fails (the database is not running, say), `secret_set` stores nothing and says why. The engines read their password variables only when the data is first made, which is why it has to happen in the database. |
| Job, `run` mode | Reads the values when each run starts: always current. |
| Job, `exec` mode | Runs in a replica: whatever the replica has. |
| Workspace (`--secret NAME`) | Never restarted (a workspace restarts only with `confirm`): `secret_set` writes the new file to `/run/isb/secrets/NAME` in each running workspace using it, and reports the workspace under `skipped`. A driver reference (`vault/item/field`) is polled every org's `secret_refresh` (`isb workspace settings --secret-refresh`, default `1h`), one lookup per item, and a new version is written the same way, with a `secret.rotated` event; `isb secret refresh REF` checks now. A program that read the old value keeps it. |
| Sandbox egress secret (`{env, secret, hosts}`) | Read by the egress proxy when a request needs it, cached for a few seconds: live. |
| `isb up` | Reads values when it runs; nothing to cycle. |

`isb secret set` and `refresh` (and the tools) report it all: per service,
its stack, the secret's key, the versions and the action taken
(`services`), the stacks that roll or restart (`rolled`), and what was not
cycled and why (`skipped`):

```console
$ printf %s "$NEW" | isb secret set smtp-password -
smtp-password: version 4
shop-production/web: web.smtp-password v3 -> v4: restarting its replicas in place
shop-production/worker: web.smtp-password v3 -> v4: not cycled (on_change: none): files updated, the app keeps v3 until it next starts
workspace main: delivered /run/isb/secrets/smtp-password; not restarted (a workspace restarts only with confirm): processes that read the old value keep it
```

### Changing the value where it is kept

Some services keep a secret's value somewhere a restart does not reset: a
database stores its users' passwords in its data, and reads the password
variable only when the data is first made. Changing the secret alone would
give the apps a password the database does not know. A top-level secret's
[`rotate`](../reference/compose.md#secretskey) command handles this: isb
runs it in a running replica of each service using the secret, with the new
value on stdin and the replica's own environment (still holding the old
value, to authenticate), before anything changes:

- `isb secret set` runs it before storing the value. If it fails, or no
  replica runs, nothing is stored and the command says why.
- A driver's new version (polled or refreshed) is taken up only after it
  succeeded; a failure leaves the stack on the version it has, with an
  `error` on each service in the answer and an `error`-level
  `secret.rotated` event, and is tried again on the next check.

[Database apps](databases.md) get it for their passwords. A compose stack
with its own database writes it, for instance for Postgres:

```yaml
secrets:
  pg_password:
    external: true
    rotate: [sh, -c, 'pw=$$(cat) && q="''" && printf ''ALTER ROLE :"u" PASSWORD :%spw%s;\n'' "$$q" "$$q" | psql -v ON_ERROR_STOP=1 -q -h 127.0.0.1 -U "$$POSTGRES_USER" -v u="$$POSTGRES_USER" -v pw="$$pw"']
```

Each service (and workspace) a new version reaches also gets a
`secret.rotated` event, `warn` when nothing restarts: it lands in the
history (`history_query`, kind `secret.rotated`) and goes to the
[notification channels](notifications.md) that want it. `stack_status`
lists, per replica, the `stale_secrets` it runs an older version of. The
`secret_set`/`secret_refresh` calls themselves are in the audit log.

A stack definition that holds base64 values in place of references is
converted when `isb serve` starts: each value moves into the org's store as
`<stack>_<key>`, the definition is rewritten with references, and the running
instances are relabelled with the new revision so nothing rolls. The daemon
logs one line per stack; starting again changes nothing.

## `isb up`

`isb up` delivers the same sources. `file:` and `environment:` it reads
itself. `external`, `age` and `driver` it reads from the org's store (`--org`,
default `default`): through `isb serve`'s `secret_resolve` tool when the
daemon answers on its socket, otherwise directly from `<state-dir>/orgs/` with
the daemon's key, found in the same order the daemon looks for it (`isb up`
never generates one). When the key exists only as the daemon's systemd
credential, the daemon must be running.

## Tools

The same operations are MCP tools on `isb serve` ([MCP
tools](../reference/mcp-tools.md)). Each takes `org` (default `default`).
Values travel base64.

| Tool | Does |
|---|---|
| `secret_create` | Create (`name`, `value`, optional `driver`, `labels`); fails if it exists. |
| `secret_set` | New value (`name`, `value`); a stack secret's `rotate` command runs first (`applied`), and if it fails nothing is stored. Returns the metadata with the new version, what each service using it does (`services`: `stack`, `service`, `key`, `secret`, `from`, `to`, `action`), the stacks that roll or restart (`rolled`), and what was not cycled and why (`skipped`). |
| `secret_get` | `{meta, value}`. |
| `secret_list` | `{secrets: [meta...], references: [...]}`, no values. Each secret carries `used_by`, the deployed stacks whose services use it; `references` are the driver references (such as `vault/item/field`) stacks use, each `{name, driver, version, used_by}`. |
| `secret_inspect` | One secret's metadata. |
| `secret_delete` | Delete, unless a deployed stack uses it. |
| `secret_refresh` | Re-read from an external source (a store name, or a stack's driver reference); answers like `secret_set`. |
| `secret_reencrypt` | Re-encrypt to the current recipients (`org`, or `all: true`). |
| `secret_recipients` | The public keys values are encrypted to. |
| `secret_resolve` | Local callers only: the values of a compose file's `external`/`age`/`driver` secrets, for `isb up`. |

Remote callers reach these tools in every org they belong to, values
included, unless the operator hides them with `--deny-tools 'secret_*'`.
Viewers and tokens scoped to `read` or `deploy` never get secret values
(`secret_get`, `secret_resolve` are refused to them).

For an org placed on a server, the secrets live on that server, encrypted to
its agent's own key ([Servers](servers.md#secrets)).
