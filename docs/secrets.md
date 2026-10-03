# Secrets

`isb serve` keeps a secret store per org: named values, encrypted at rest,
that you manage with `isb secret` or the `secret_*` tools. Stacks refer to a
stored secret by name from their compose file (`external: true`).

```sh
printf %s "$DB_PASSWORD" | isb secret create db_password    # value from stdin
isb secret set db_password ./new-password.txt               # or from a file: version 2
isb secret ls
isb secret inspect db_password                              # metadata, never the value
isb secret get db_password                                  # the value, raw, to stdout
isb secret rm db_password
```

Every command talks to the daemon over its unix socket, as `isb stack` does,
and takes `--org ORG` (default `default`).

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

`isb secret refresh NAME` re-reads a secret from its source; for `local` it is
a no-op.

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
| `driver` + `name` | A secrets driver, by that driver's reference. |

```yaml
secrets:
  db_password: {external: true}
  tls_key: {external: true, name: web.tls-key}
  api_token: {age: "..."}
services:
  web:
    image: dev-base
    secrets: [db_password, {source: tls_key, target: key.pem}]
```

`external`, `age` and `driver` are accepted and validated; `isb up` and `isb
stack deploy` do not deliver them yet, and a service that uses one fails to
deploy with a message saying so. `isb secret rm` refuses to delete a secret
that a deployed stack's services use through `external: true`.

## Tools

The same operations are MCP tools on `isb serve` (see [serve.md](serve.md)).
Each takes `org` (default `default`). Values travel base64.

| Tool | Does |
|---|---|
| `secret_create` | Create (`name`, `value`, optional `driver`, `labels`); fails if it exists. |
| `secret_set` | New value (`name`, `value`); returns the metadata with the new version. |
| `secret_get` | `{meta, value}`. |
| `secret_list` | `{secrets: [meta...]}`, no values. |
| `secret_inspect` | One secret's metadata. |
| `secret_delete` | Delete, unless a deployed stack uses it. |
| `secret_refresh` | Re-read from an external source. |
| `secret_reencrypt` | Re-encrypt to the current recipients (`org`, or `all: true`). |
| `secret_recipients` | The public keys values are encrypted to. |

Remote callers (through Cloudflare Access) see these tools unless
`--deny-tools` hides them, for example `--deny-tools 'secret_*'`, and they
reach every org's secrets.
