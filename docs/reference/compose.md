---
title: isb.yaml reference
description: Every field of the compose file, how variables are filled in, and exactly what isb up changes on an existing sandbox.
order: 2
nav_title: isb.yaml
---

A compose file describes named storage volumes and any number of services, each
one sandbox (an incus container or VM). The format is docker compose's wherever
incus allows; "Differences from docker compose" lists where it is not. `isb up`
creates what is missing and reconciles what exists, changing only what differs. isb talks to incusd over its unix socket
(`--socket`, else `$INCUS_SOCKET`, else `$INCUS_DIR/unix.socket`, else
`/var/lib/incus/unix.socket`).

## Files and validation

- With no `-f`, isb looks in the current directory for `isb.yaml`, then
  `isb.yml`, and merges `isb.override.yaml` (or `isb.override.yml`) over it if
  one exists. Only that directory is searched, never a parent; with no file
  there, the command fails saying so.
- `-f FILE` may be repeated, and then no override file is loaded. It is accepted
  before the subcommand and after the compose-aware ones (`up`, `down`, `plan`,
  `config`, `ps`, `inspect`, `exec`, `logs`, `stack deploy`):
  `isb -f a.yaml up -f b.yaml` loads `a.yaml` then `b.yaml`.
- Files are merged in order, as docker compose merges them. Mappings merge key
  by key, recursively; `environment` and `labels` merge by key whichever form
  they are written in. A service's `ports` are appended (an identical entry is
  kept once), and its `volumes` merge by target, a later mount replacing an
  earlier one at the same target. Everything else (scalars and other lists,
  such as `command`, `ready`, `incus_profiles`) is replaced by the later file.
- Each file is also validated on its own, so an error names the file.
- Relative bind paths resolve against the directory of the first file, including
  mounts declared in later files.
- Every object rejects unknown fields, so a typo is an error. A docker compose
  key isb has no equivalent for (`build`, `networks`, `env_file`,
  ...) is an error that says what to use instead. The obsolete
  top-level `version` is ignored. Keys starting with `x-` are dropped at the top
  level and directly inside each service (and only there). They are useful as
  YAML anchor holders. YAML anchors, aliases and `<<` merge keys work within one
  file.

To check a file without touching anything:

- `isb config` prints the resolved file (interpolated, merged, project name and
  sandbox names filled in). `isb config --services` prints the service names.
- `isb plan` shows what `up` would change (see "How reconcile works").
- `isb schema` prints the JSON Schema of the format.

### Value types

Interpolation produces strings, so typed fields also accept strings:

- Booleans (`privileged`, `read_only`, `external`, `login`) accept `true`/`false`
  or a string: `true`, `yes`, `on`, `1`, `false`, `no`, `off`, `0` or empty,
  case-insensitive.
- `cpus`, `cpuset`, `mem_limit`, `ready_timeout`, `user`, a mount's `owner` and
  a port's `target` and `published` accept a non-negative integer or a string.
  Floats are rejected.
- Durations (`ready_timeout`): a number with an optional unit `ms`, `s`/`sec`/
  `secs`, `m`/`min`/`mins`, `h`, `d`. No unit means seconds. Decimals work (`1.5m`).
- The values of free-form string maps (`labels`, `environment`, `raw_config`,
  the properties in `raw_devices`, mount and port `options`, top-level volume
  `config`, `exec.env`) may be any scalar and are stored as its string form:
  `environment: {DEBUG: 1}` is `"1"`, `raw_config: {security.nesting: true}` is
  `"true"`. A float is written in its shortest form (`1.0` becomes `"1"`), so
  quote a value whose exact spelling matters. Map keys must be strings.
- `environment`, `exec.env` and `labels` may also be written as a list of
  `KEY=VALUE` strings. In an environment, a bare `KEY` takes its value from the
  variables used for interpolation and is left out when unset, as in docker. A
  bare label is empty.
- Other string fields (`container_name`, `image`, `storage`, `working_dir`,
  ...) take YAML strings only.

## Differences from docker compose

The format follows docker compose; these are the places it does not, each on
purpose.

- **Ports listen on `127.0.0.1` by default,** not `0.0.0.0`. Publishing to every
  interface by accident is a well-known way to expose a dev server. Write the
  address to publish elsewhere: `"0.0.0.0:8080:80"`.
- **A port needs its host side.** `"80"` alone means a random host port in
  docker; isb has no random ports, so it is an error.
- **An unset variable is an error,** where docker substitutes an empty string
  with a warning. A blank `container_name` or mount path does damage quietly.
  `${VAR:-}` allows empty.
- **The file is `isb.yaml`,** so it can sit next to a docker project's
  `compose.yaml` without either tool reading the other's.
- **`ready` as well as `healthcheck`.** isb's `ready` checks gate `up` once, and
  include incus-specific ones (`default_route`, `user_exists`, `path_writable`).
- **incus keys keep incus names:** `type: vm`, `storage`, `idmap`,
  `incus_profiles`, `incus_project`, `raw_config`, `raw_devices`, and the
  `listen`/`connect` port form. docker's `profiles` (service activation) and
  `name`'s role as the project are left to mean what they mean in docker.
- **A long-syntax mount may omit `type`;** it is inferred from `source`, as in
  the short syntax.
- **No images are built, and there are no networks or `configs`.** Those keys
  are errors that say what to use instead.
- **`restart` and `deploy` mean what they mean to docker, split by command.**
  `isb up` honours `restart` (the app is supervised in the guest) and refuses
  more than one replica; `isb stack deploy` honours all of `deploy` and, like
  swarm, takes its restart behaviour from `deploy.restart_policy`.
- **On an OCI image, `command` replaces the whole command line,** including the
  image's entrypoint, since incus does not expose the two separately. Set
  `entrypoint` too to keep the image's.


## Interpolation

Variables are expanded in every string scalar and every mapping key of the
parsed YAML tree, never in the raw text, so a value cannot inject YAML structure.
Numbers and booleans written unquoted are left alone. Expansion happens per file,
after merge keys are applied and `x-` keys are dropped, and before files are
merged. An unreferenced `x-` block is therefore never expanded; one pulled in with
`<<` is expanded where it lands.

| Syntax | Result |
|---|---|
| `$VAR`, `${VAR}` | The value. Unset is an error. Set but empty gives `""`. |
| `${VAR:-default}` | `default` if unset or empty. |
| `${VAR-default}` | `default` if unset only. |
| `${VAR:?message}` | Error with `message` if unset or empty. |
| `${VAR?message}` | Error with `message` if unset only. |
| `${VAR:+alt}` | `alt` if set and non-empty, else `""`. |
| `${VAR+alt}` | `alt` if set (even empty), else `""`. |
| `$$` | A literal `$`. |

- An unset variable with no default is an error, not an empty string. Use
  `${VAR:-}` to allow empty.
- `default`, `message` and `alt` may contain interpolations themselves.
- A `$` not followed by `$`, `{`, a letter or `_` is literal (`5$`, `$(cmd)`).
- Names are `[A-Za-z_][A-Za-z0-9_]*`. Other `${...}` forms, such as
  `${VAR/x/y}` or an unterminated `${`, are errors.
- With an empty message, `${VAR:?}` reports `VAR is required`.

Variable sources: the process environment, then `--env-file FILE` (repeatable),
or, without `--env-file`, a `.env` file next to the first compose file if there
is one. The process environment wins; a later env file overrides an earlier one. An env
file has `KEY=VALUE` lines, blank lines and `#` comment lines are skipped, an
`export ` prefix is allowed, one pair of matching `"` or `'` around the value is
removed, and nothing is expanded. Inline comments are not stripped. A non-blank
line without `=` is an error.

## Top-level fields

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | directory of the first file | Compose project name. |
| `incus_project` | string | `default` | incus project to operate in. |
| `volumes` | map of key to volume | `{}` | Named custom storage volumes. |
| `secrets` | map of key to secret | `{}` | Secrets services can mount as files. |
| `services` | map of service to sandbox | `{}` | The sandboxes, keyed by service name. |

`name` is sanitized: lowercased, each run of characters outside `[a-z0-9]`
becomes one `-`, leading and trailing `-` are trimmed, and `isb-` is prepended if
the result does not start with a letter (an empty result becomes `isb`).
`-P/--project-name` overrides it. The default sandbox name is
`<name>-<service>`, with the service key sanitized the same way (`Web_1` in
project `lasso` becomes `lasso-web-1`). The default volume name is
`<name>_<key>`.

`incus_project` is overridden by `--project` (or `$INCUS_PROJECT`), and by
`--org`, whose incus project it then is. It also scopes the
per-sandbox lock (see "The ensure flow").

## `volumes.<key>`

A named custom storage volume (filesystem content type). A service mounts it by
its key, and may only mount a volume declared here. A declared volume that no
service mounts is never created.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | `<project>_<key>`; the key if `external` | The incus volume name. Set it to share one volume across projects. |
| `pool` | string | `auto` | Storage pool. `auto` means the root pool of the sandbox that mounts it. A named pool must exist. |
| `config` | map of string | `{}` | Volume config keys (`size: 10GiB`). Applied only when isb creates the volume; never compared or changed later. |
| `external` | bool | `false` | The volume must already exist. isb never creates it; `plan` and `up` fail if it is missing. |

A volume is created before the sandbox that mounts it. If two sandboxes resolve
the same volume to different pools, each pool gets its own volume.

## `secrets.<key>`

A secret a service uses as a file (its `secrets`) or a variable
(`environment: {KEY: {secret: NAME}}`). Exactly one source:

| Field | Meaning |
|---|---|
| `file` | A host file holding the value, relative to the compose file. Read by whoever runs `isb up` or `isb stack deploy`. |
| `environment` | An environment variable (or `--env-file` / `.env` entry) holding it. Read the same way. |
| `external` | `true`: the org's secret store on `isb serve`, under `name` (default: the key). |
| `age` | The value, age-encrypted to the daemon's recipients (`isb secret encrypt`), decrypted with the daemon's key. |
| `driver` | A secrets driver; `name` is the driver's reference. |
| `refresh` | With `driver`: how often `isb serve` checks it for a new version (`30m`; default `1h`, at least `10s`). |
| `on_change` | What a new version does to the stack services using it: `roll` (default), `restart` or `none`. Any source. A service's own reference overrides it. |
| `rotate` | Under `isb serve`: argv run in a running replica of each service using it when it gets a new version, before any replica is given it, with the new value on stdin and the replica's environment (holding the old value). For values a service keeps itself, such as a database user's password. A failure stops the change: `isb secret set` stores nothing, a driver's new version is not taken up. See [Changing the value where it is kept](../guides/secrets.md#changing-the-value-where-it-is-kept). |

`name` goes with `external` or `driver` only. A secret no service uses is never
read. A deployed stack keeps references to the org's store (name and version),
never values; see [Secrets](../guides/secrets.md#stacks).

```yaml
secrets:
  db_password: {environment: DB_PASSWORD}
  tls_key: {file: ./certs/key.pem}
  api_token: {external: true}
```

### `on_change`

Under `isb serve`, a secret's new version (`isb secret set`, or a driver's
version moving) reaches each service using it in one of three ways:

| Value | What happens to the service's replicas |
|---|---|
| `roll` (default) | A new revision: a rolling update replaces them, per `deploy.update_config` (`order`, `parallelism`, `delay`, `monitor`, `failure_action`), each new replica health-checked before the next. |
| `restart` | Each keeps its instance: it is drained from the load balancer, gets the new files and variables, its app is restarted, and it must serve again (health check, then `update_config.monitor`) before the next batch of `update_config.parallelism`. A replica that fails stops the rest, which keep the old value until the next version or deploy; the service's message says so. |
| `none` | Nothing restarts. The new value goes where a running replica can take it: its files under `/run/secrets` (and their boot copies), and the variables its next start reads (the unit's environment file, or an OCI instance's config). The app keeps the old value until it next starts, and each replica's `stale_secrets` in `stack_status` says which secrets it runs an older version of. |

A service's own reference sets it for that service: `secrets: [{source:
KEY, on_change: ...}]`, or `environment: {VAR: {secret: KEY, on_change:
...}}`. When a service uses a secret more than once, the strongest setting
wins (`roll`, then `restart`, then `none`). `isb up` has no daemon and
ignores it.

A `roll` secret's version is part of the service's revision; a `restart` or
`none` secret's is not. So changing a secret between `roll` and the other
two rolls its services once, on that deploy; moving between `restart` and
`none` does not. The versions a replica's app started with are kept on its
instance (`user.isb.secrets`), so a daemon restart forgets nothing.

```yaml
secrets:
  db_password: {external: true, on_change: restart}
  feature_flags: {external: true, on_change: none}   # the app re-reads the file
services:
  api:
    secrets: [db_password, feature_flags]
    environment:
      SMTP_PASSWORD: {secret: smtp, on_change: none}
```

## `services.<service>`

Each field below lists its type, default, what it becomes in incus, and whether
`isb up` reconciles it on an existing instance or only uses it at creation.
"Reconciled" for config keys means the key is set when its value differs. isb
never unsets a key: removing a field, label or env var from the file leaves the
old key on the instance.

### `container_name`

String. Default `<project>-<service>` (also when empty). The incus instance name:
at most 63 characters of `[A-Za-z0-9-]`, starting with a letter, not ending with
`-`. An explicit name is validated, not sanitized. Fixed: a different name is a
different instance.

### `image`

String, required. Either a local image alias or fingerprint (`dev-base`, or a
fingerprint prefix of at least 12 hex digits), or `remote:alias` for one of these
remotes, pulled over simplestreams:

| Prefix | Server |
|---|---|
| `images:` | `https://images.linuxcontainers.org` |
| `ubuntu:` | `https://cloud-images.ubuntu.com/releases` |
| `ubuntu-daily:` | `https://cloud-images.ubuntu.com/daily` |
| `ubuntu-minimal:` | `https://cloud-images.ubuntu.com/minimal/releases` |

Or an OCI (docker) image, pulled from a registry:

| Prefix | Registry | Example |
|---|---|---|
| `docker:` | `https://docker.io` | `docker:nginx:1.27`, `docker:traefik/whoami` (Docker Hub's `library/` and `:latest` are filled in) |
| `ghcr:` | `https://ghcr.io` | `ghcr:umami-software/umami:3.0.3` |
| `quay:` | `https://quay.io` | `quay:prometheus/node-exporter` |
| `oci:` | `https://REGISTRY` | `oci:public.ecr.aws/nginx/nginx:1.27` (not a loopback registry) |
| `registry:` | the host's local registry, in this org | `registry:web:v1`, `registry:web@sha256:...` |

After the prefix comes Docker's own reference, so a colon is a tag:
`docker:traefik:whoami` is the image `library/traefik`, tag `whoami`; the image
`traefik/whoami` is `docker:traefik/whoami`. Through `isb serve`, a stack
deploy looks each new registry image up first and refuses one its registry
does not have ([Image references](../guides/deploy-apps.md#image-references)).

`registry:APP[:TAG][@sha256:DIGEST]` is an image the org built
([Builds](../guides/builds.md)): always the repository `<org>/APP` of the org the
sandbox or stack is in, so another org's images cannot be named. A stack
resolves its tags to digests when it is deployed, so a moved tag is an update
and a rollback runs the old digest.

An OCI image runs as an application container: its process is the instance's
init, its stdout and stderr are the console log (`isb logs`), and it stops when
the process exits. It needs `skopeo` on the host and incus with the
`instance_oci` API extension (incus 6.3 or later). On one:

- `command` (with `entrypoint`, if set) becomes `oci.entrypoint`, the whole
  command line. It is instance config, reconciled like any other key, and takes
  effect on restart.
- `working_dir` becomes `oci.cwd`, and `user` must be numeric (`1000` or
  `1000:1000`), becoming `oci.uid`/`oci.gid`.
- It must be a container (`type: vm` is an error).

The string is split at the first `:`, so any other prefix is an error and a local
alias cannot contain `:`. A local image that does not exist is an error before
anything is created. For a virtual machine the image must be a VM image. Used
only at creation, never changed on an existing instance. `plan` adds a note when
a local alias now points to a different fingerprint than the instance's
`volatile.base_image` ("recreate to pick it up"); a change of remote image, or of
the `image` string itself, produces no note.

```yaml
image: images:debian/12
```

### `type`

`container` (default) or `virtual-machine`, with `vm` accepted as an alias.
The CLI equivalent is `isb create --vm`. Sent as the instance type in the create
request. Fixed at creation; a mismatch on an existing instance is reported as a
note. A VM changes what several other fields accept; see "Containers vs virtual
machines".

```yaml
type: vm
```

### `storage`

String, default `auto`. The pool for the root disk. `auto` (or empty) picks
`incus-zfs` if it exists, else `default`, else the first pool; no pools at all is
an error. A named pool must exist. Becomes the `root` device
`{type: disk, path: /, pool: <pool>}` in the create request. Fixed at creation; a
root disk on another pool is reported as a note. This resolved pool is also the
`auto` pool for named volumes the sandbox mounts.

### `cpus`

Whole number, optional. Becomes `limits.cpu` (`8`): incus pins the instance to
that many CPUs and balances it across them. Fractions (docker's `0.5`) are
rejected; for a CPU-time cap set `raw_config: {limits.cpu.allowance: 50%}`.
Reconciled, live.

### `cpuset`

String, optional. Becomes `limits.cpu` as a CPU set (`0-3`, `0,2`). Setting both
`cpus` and `cpuset` is an error. Reconciled, live.

### `mem_limit`

Size, optional. Becomes `limits.memory`. Docker's units are translated, binary
as docker reads them: `512m` is `512MiB`, `8g` and `8GB` are `8GiB`, and a bare
number is bytes. incus' binary units (`8GiB`) and percentages (`50%`) pass
through. Sizes are whole numbers (`1536m`, not `1.5g`). Reconciled, live.

### `privileged`

Boolean, optional. Becomes `security.privileged` (`"true"`/`"false"`). Omit it to
leave the incus default (unprivileged); `false` pins it. Reconciled; takes effect
on restart. Container-only: setting it at all (even `false`) on a VM is an error.

### `idmap`

Optional. Decides `raw.idmap`, usually so host uid/gid 1000 is guest 1000 and a
bind-mounted checkout is writable. Forms:

```yaml
idmap: auto          # map 1000 <-> 1000 only where this host needs it
idmap: none          # never set raw.idmap
idmap: always        # always map 1000 <-> 1000
idmap: {}            # same as auto
idmap: {mode: auto, host_uid: 1001, host_gid: 1001, guest_uid: 1000, guest_gid: 1000}
idmap: {raw: "both 1000 1000"}   # raw.idmap verbatim
```

In the map form, `mode` is `auto` (default), `always` or `none`, and each id
defaults to `1000`. A raw value must use `{raw: ...}`; a bare string other than
the three modes is an error.

The `auto` rule, computed on this host at every plan: a mapping is needed for an
id unless that id lies inside a subordinate id RANGE owned by `root` (or `0`),
that is a line `root:START:COUNT` in `/etc/subuid` (for the uid) or `/etc/subgid`
(for the gid) with `COUNT > 1` and `START <= id < START + COUNT`. A `root:1000:1`
delegation line is not a range and does not count. A missing file counts as no
range. uid and gid are decided separately. On macOS (and inside its
`isb machine`, which sets `ISB_BIND_CALLER_OWNED=1`) `auto` never maps: bind
sources are the Mac home over virtiofs, which every guest uid can already
write ([isb on macOS](../getting-started/macos.md#users-and-file-ownership)).

The value is `both H G` when uid and gid need mapping with the same pair,
otherwise one line per id that needs it: `uid HOST GUEST` and/or
`gid HOST GUEST`, joined by a newline.

Reconciled as `raw.idmap`; takes effect on restart. isb never removes the key.
If the instance has a `raw.idmap` that the spec does not produce, `plan` notes it
("not needed on this host" for `auto`, "the spec says idmap: none" for `none`)
and leaves it to be unset by hand.

**On a VM** the same key makes the host bind mounts safe rather than writable;
see [Host directories in a VM](#host-directories-in-a-vm). Unset, `auto`,
`always` and the map form map the service user (see below) to the user running
isb, and `idmap: none` shares untranslated (unsafe, with a warning). The
`/etc/subuid` rule above does not apply to a VM.

### Host directories in a VM

A host bind mount reaches a VM over virtiofs, which by itself passes ids through
untouched: guest root would create root-owned files, with setuid bits and
device nodes, in the host directory. isb therefore sets `raw.idmap` on every VM
that has a host bind mount, whether or not the spec says `idmap`. incus (7.5 or
later) then starts that mount's virtiofsd on the host with
`--translate-uid`/`--translate-gid`, a map the guest cannot remount or change.

**Guarantee** (checked against incus 7.5.1 by `tests/integration.rs`
`virtual_machine`): through a bind mount, exactly one guest uid and one guest gid
can read or write host files, and they appear on the host as the user (and
group) running isb. Every other guest id, root included, gets an error when it
creates a file, chowns one, or makes a device node, so no file on the host is
ever owned by an id the sandbox's owner does not control.

**What it does not stop:** a setuid or setgid bit can still be set on a file the
mapped id owns. That file belongs to the invoking user on the host, so running
it there gains nothing the user did not have, except for other host users who
execute it as that user. To close even that, keep the shared directory on a
`nosuid` host mount, or mount it read-only (`:ro`): a guest-side `nosuid` does
not count, because guest root can remount it.

Which guest id is mapped: `idmap: {mode: always, guest_uid: N, guest_gid: M}` if
you say so; otherwise the service user: a numeric `user:` (`1000` or `1000:50`),
root when `user:` is unset or `root`, and 1000 for a named user (dev-base's
`dev`). The host side defaults to the uid and gid of the process running isb,
not 1000. `{raw: ...}` is passed to incus verbatim. A map is one guest id per
host id: mapping the same host id to two guest ids (root and a service user)
makes virtiofsd fail to start the VM, so only one of them can write.

isb refuses to create or reconcile such a VM on an incus older than 7.5 or whose
version it cannot read: an older incus may ignore `raw.idmap` for VMs and share
the directory untranslated. Upgrade incus, or set `idmap: none` to accept that
(isb prints a warning on every plan). VMs with no host bind mount, only named
volumes, get no `raw.idmap`. Changing the map restarts the VM
(`raw.idmap` cannot change while it runs).

### `incus_profiles`

List of strings, default `[default]`. Applied in order at creation. Fixed at
creation; a different list (order counts) is reported as a note.

### `labels`

Map of string to string, or a list of `KEY=VALUE`. Each becomes config key
`user.<key>`. A key must be
non-empty and contain no whitespace. Reconciled, live. Labels not in the file are
never removed. Used by `isb ls --label key[=value]` and
`isb prune --label key --missing-path`. isb itself writes
`user.isb.create-token` at creation (see below); `user.isb.*` keys are
bookkeeping and are not shown as labels.

```yaml
labels: {app: web, worktree: "${WORKTREE}"}
```

### `environment`

Map of string to string, or a list of `KEY=VALUE`. Each becomes
`environment.<KEY>`, which incus applies to every exec. Reconciled. This is plain instance config, readable by anyone who can
read the instance: never put a secret value here.

A value may instead be `{secret: NAME}`, a top-level secret delivered as the
variable (map form only):

```yaml
environment:
  LOG_LEVEL: info
  API_TOKEN: {secret: api_token}
```

- **System image:** the variable goes only into the supervised command's 0600
  environment file (`/etc/isb/<service>.env`), and to a foreground `isb up`
  command through exec, never into instance config. The service needs a
  `command`.
- **OCI image:** the process is the instance's init, so the variable is
  instance config, `environment.API_TOKEN`: **plaintext in the incus
  database**, readable by anyone who can read the instance's config (`incus
  config show`). isb never shows it in plans or reports (`(secret)`), and
  `isb stack deploy` and `isb up` warn once per deploy, naming each such
  variable. Use `as: file` (below) instead.

`{secret: NAME, as: file}` keeps the value out of instance config: it is
written to the file `/run/secrets/NAME` (mode `0400`, owned by the user the
app starts as: a numeric `user`, else the OCI image's own user), and the
variable `KEY_FILE` holds that path. Postgres, MariaDB, MySQL and many other
images read `KEY_FILE` in place of `KEY`; for an app that does not, read the
file in its entrypoint. On an OCI image the file is written before the
instance's first start, so the app finds it as it starts. Setting `KEY_FILE`
yourself as well is an error. `as: env` is the default; changing a variable
between the two is a new revision.

```yaml
environment:
  POSTGRES_PASSWORD: {secret: db_password, as: file}   # POSTGRES_PASSWORD_FILE=/run/secrets/db_password
```

For another path, owner or mode, mount the secret with the service's
[`secrets`](#secrets) and set `KEY_FILE` to its path as a plain value.

A variable's value (`as: env`) must be text (UTF-8, no NUL). A new version of the secret reaches a
stack's replicas per its [`on_change`](#on_change) (default: replaced by a
rolling update); `{secret: NAME, on_change: restart}` sets it for this
variable.

### `volumes`

List of mounts, in docker's short or long syntax. The same target twice (a
trailing `/` ignored) is an error.

**Short syntax:** `SOURCE:TARGET[:OPTIONS]`. A `SOURCE` starting with `/`, `.`
or `~` is a host path (a bind mount); anything else is the key of a named volume.
`TARGET` is absolute. `OPTIONS` is a comma list of `ro`, `rw`, docker's
propagation modes (`shared`, `rslave`, ...), `z`/`Z` (ignored), and isb's
`owner=USER`, `device=NAME`, `pool=POOL`, `external`, and docker's `nocopy`. There are no anonymous
volumes, so a bare `TARGET` is an error.

**Long syntax:**

| Field | Applies to | Type | Default | Meaning |
|---|---|---|---|---|
| `type` | both | `bind` or `volume` | from `source`, as in the short syntax | |
| `source` | both | string | required | Host path, or the key of a named volume. |
| `target` | both | string | required | Absolute path inside the guest. |
| `read_only` | both | bool | `false` | Mount read-only (`readonly: "true"`). |
| `external` | volume | bool | `false` | The volume must already exist. |
| `pool` | volume | string | see below | Pool of the named volume. |
| `owner` | volume | string or int | | chown the mount point in the guest. |
| `volume.nocopy` | volume | bool | `false` | Do not seed the volume from the image (see below). |
| `device` | both | string | derived | incus device name. |
| `options` | both | map of string | `{}` | Extra disk device properties (`shift`, `propagation`, ...), verbatim. |

`owner`, `pool`, `external` and `volume.nocopy` on a bind mount are errors (isb
never chowns host paths).

**Device.** A bind mount becomes `{type: disk, path: GUEST, source: HOST}`; a
named mount becomes `{type: disk, path: GUEST, pool: POOL, source: NAME}`, plus
`readonly: "true"` if set, then `options`. An option named `type`, `path`,
`source`, `pool` or `readonly` is an error ("would override a core property");
use the field instead. Every mount is in the create request, so it exists before
first boot. Reconciled per the device rules in "How reconcile works"; replacing a
disk remounts it. In a VM, bind mounts are shared over virtiofs (see "Containers
vs virtual machines").

**Default device name.** The guest path lowercased, each run of characters
outside `[a-z0-9]` turned into one `-`, trimmed (`/home/dev/.bun/install/cache`
becomes `home-dev-bun-install-cache`; `/` becomes `mount`). A name longer than 48
characters becomes its last 39 characters plus `-` and an 8-hex-digit FNV-1a
hash of the path. Set `device` to adopt an existing device under a known name.

**Bind source resolution.** `~` and `~/...` expand to `$HOME`. A relative path
resolves against the compose file's directory. The path is then canonicalized
(symlinks resolved); a missing path is an error. Finally it is translated for
incusd, whose mount view can differ from isb's:

- `ISB_HOST_PATH_MAP=FROM=TO` rewrites a source equal to `FROM`, or starting
  with `FROM/`, to use `TO` instead (trailing slashes on both are ignored). If
  the variable is set but malformed, no translation happens at all.
- Otherwise, if `/etc/workspace/guest-home` exists (inside an agent workspace
  box), `FROM` is `$HOME` and `TO` is that file's content.

The prefix must end at a path boundary: with `FROM=/home/u`, `/home/user2` is
left alone.

**Named volume name.** The top-level volume's `name` (`<project>_<key>` by
default). In a bare spec over RPC or the library, with no top-level volumes, the
source is the incus volume name itself.

**Named volume pool.** The mount's `pool`, else the top-level volume's `pool`,
else `auto`. `auto` means this sandbox's root pool (`storage`). A named pool must
exist.

**`external`.** True on either the mount or the top-level volume makes the
volume required: `plan` and `up` fail if it does not exist.

**Seeding.** As in docker, a named volume that is empty the first time it is
used starts as a copy of what the image has at the target, ownership included,
so a seeded mount needs no `owner`. isb adds `initial.copy: "true"` to the
device when the instance is a container and the server has the
`disk_initial_copy` API extension; `nocopy` leaves it off. On an older server,
or in a VM, the volume is mounted empty. Because the key only acts on first use,
a disk that differs from the spec in nothing but `initial.copy` is correct and
is not replaced, so upgrading isb or incus never remounts an existing volume.

**`owner`.** `USER`, `USER:GROUP`, or a numeric uid (`1000`, `1000:1000`). After
the volume is attached, isb runs a script in the guest as root:

1. Look up `USER` with `getent passwd`. If absent, it must be numeric, and then
   uid and gid are that number and the home is empty.
2. The group is `GROUP` if given (passed to `chown` verbatim, name or number),
   else the user's primary gid.
3. `chown` the mount point (not recursive).
4. If the user has a home other than empty or `/`, and the mount point is
   inside it, walk up its parents, chowning each one owned by uid 0, and stop at
   the home, at `/`, or at the first parent not owned by root. A numeric owner
   with no passwd entry has no home, so no parent is touched.

It runs after the first start of a new instance, and on an existing instance only
when that mount's device was just added or replaced. A correct device is not
chowned again. On a VM, isb first waits (up to `ready_timeout`) for the `agent`
check, since the chown runs through the incus agent.

```yaml
volumes:
  - ./src:/home/dev/src:device=src
  - dev-cache:/home/dev/.cache:owner=dev
  - {type: bind, source: ~/ref, target: /srv/ref, read_only: true, options: {shift: "true"}}
```

### `ports`

List of incus proxy devices: ports published from the guest in docker's short or
long syntax, or a proxy in either direction in incus' own terms.

**Short syntax:** `[HOST_IP:]PUBLISHED:TARGET[/PROTOCOL]`, as in docker, except
that `HOST_IP` defaults to `127.0.0.1` rather than `0.0.0.0`. IPv6 hosts go in
brackets (`[::1]:8080:80`). A bare `TARGET` is an error: docker would pick a
random host port, and isb needs to know it.

**Long syntax:**

| Field | Type | Default | Meaning |
|---|---|---|---|
| `target` | port or range | required | Port in the guest. |
| `published` | port or range | required | Port on the host. |
| `host_ip` | string | `127.0.0.1` | Host address to listen on. |
| `protocol` | `tcp` or `udp` | `tcp` | |
| `name` | string | derived | incus device name. |
| `options` | map of string | `{}` | Extra proxy properties (`proxy_protocol`, ...), verbatim. |

**Ranges.** A `published` range with a single `target` (`5173-5223:5173`) takes
the first free host port in the range, as docker does; see "Searched ports"
below. Two ranges of the same length (`8000-8010:9000-9010`) map port to port.
A `target` range with a single or different-length `published` is an error.

**incus form,** for anything the docker forms cannot say, such as a guest that
reaches a host service:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | derived | Device name. |
| `bind` | `host` or `guest` | `host` | `host`: listen on the host, connect in the guest (publish a guest port). `guest`: listen in the guest, connect on the host (reach a host service). |
| `listen` | string | required | An address (see below). |
| `connect` | string | required | An address (see below). |
| `options` | map of string | `{}` | Extra proxy properties (`nat`, `proxy_protocol`, ...), verbatim. |

A port with `listen` or `connect` is in the incus form, where a range keeps
incus' meaning (a listen range forwards to a connect port or range), never a
search.

**Addresses** (incus form). The protocol defaults to `tcp` and the host to
`127.0.0.1`:

| Written | Means |
|---|---|
| `5173` | `tcp:127.0.0.1:5173` |
| `0.0.0.0:5173` | `tcp:0.0.0.0:5173` |
| `5353/udp` | `udp:127.0.0.1:5353` |
| `tcp:5173`, `udp:5353` | `tcp:127.0.0.1:5173`, `udp:127.0.0.1:5353` |
| `tcp:HOST:PORT`, `udp:HOST:PORT`, `unix:PATH` | as written |

**Device.** Every form becomes `{type: proxy, bind, listen, connect,
...options}`, with the guest side (`connect` of a published port) on
`127.0.0.1`. On a VM the guest side defaults to host `0.0.0.0` instead, which
lets incus' NAT mode find the VM's address, and `nat: "true"` is added before
`options`. The port may be a range or a list, as incus allows (`8000-8010`,
`80,443`). isb stores the full form, so `5173:5173` in the spec matches an
existing device written as `listen: tcp:127.0.0.1:5173`, `connect:
tcp:127.0.0.1:5173`, and reconcile leaves it alone. An option named `type`,
`bind`, `listen` or `connect` is an error ("would override a core property").
Reconciled per the device rules. On a VM, `bind: guest` is an error.

**Default name.** `port-<bind>-<port>` for TCP (`port-host-5173`),
`port-<bind>-udp-<port>` for UDP, and `port-<bind>-<derived>` for unix sockets,
where `<derived>` is the device-name rule applied to the whole address
(`unix:/run/app.sock` gives `port-guest-unix-run-app-sock`). For a published
range it is the first port of the range.

**Searched ports.** A published range with a single target is not in the create
request. It is added after the instance is running: for each port of the range
in order, isb first tries to bind it locally (TCP only) and skips it if it is in
use, then adds the device, and moves to the next port if incus refuses. `isb up`
prints `<service> <device> <listen>` for each such port. An existing device with
that name whose listen port is anywhere in the range (same protocol and address,
every other property equal) counts as correct. One that is not (outside the
range, or otherwise different) is removed and searched for again after the
start, rather than replaced at a port that may be taken.

**Under a stack** (`isb stack deploy`), host-bound ports are not proxy
devices: TCP is served by the daemon's balancer and UDP by a NAT proxy on
the service's one replica, single ports only (see [Stacks](../concepts/stacks.md#the-load-balancer)).

```yaml
ports:
  - "${IP}:5173-5223:5173"
  - {name: api, target: 8000, published: 8001}
  - {name: backend, bind: guest, listen: 8190, connect: 8080}
```

### `ready`

List of readiness checks, default `[running]` for a container and
`[running, agent]` for a VM. Run in order after `isb up` and `isb create` (skip
them with `--no-ready`); each check is retried every 250 ms until it passes or
the shared deadline (`ready_timeout`) runs out, and always gets at least one try.
Each probe command is capped at 20 s.

A stopped instance does not get ready by waiting. While any check has not
passed, isb also looks at the instance: once it has been neither Running nor
Starting for 30 s, isb starts it once more (incus 7.0.1 sometimes fails to complete a
guest-initiated reboot, which is common on a VM's first boot with cloud-init).
If it stops again for 30 s, or the start fails, readiness fails at once with
"stopped while getting ready" instead of at the deadline. The extra start
happens at most once per readiness wait.

| Form | Passes when |
|---|---|
| `running` | incus reports the instance Running. |
| `agent` | `true` runs in the guest through the incus agent (a VM's agent answers exec). Passes as soon as a container is running. |
| `default_route` | The guest has a default route: a line in `/proc/net/route` with destination `00000000` and the up flag, or a line in `/proc/net/ipv6_route` with an all-zero destination, prefix length `00`, on a device other than `lo`. Read as root. |
| `{user_exists: USER}` | `getent passwd USER` exits 0 (run as root). |
| `{path_writable: PATH}` | `test -w PATH` exits 0, run as `user` (else root), from that user's home. |
| `{command: [ARGV...]}` | The argv exits 0, run as root with no shell and no `exec` defaults. Non-string items are stringified (`[true]` is `["true"]`). |

"Running" alone is not ready: networking comes up a moment after the instance
does. Not stored in incus.

```yaml
ready: [running, default_route, {user_exists: dev}, {path_writable: /home/dev/src}]
```

### `ready_timeout`

Duration, default `60s` for a container and `300s` for a VM (a container is
usable about a second after Running; a VM boots a kernel and its agent, 50 to
90 s under nested virtualization). One deadline for all checks together, measured from the
start of the first. Not stored in incus.

### `user`

String or integer, optional. The guest user for `command`, `isb exec SERVICE`
and the `path_writable` check: `NAME`, `UID`, `UID:GID` or `NAME:GROUP`. Default
root. Per-call `isb exec -u` overrides it. Client-side only; nothing in incus
changes.

User resolution, in the guest, as root:

- `UID` or `UID:GID` (numeric): those ids, with no lookup, so an image
  without `getent` works. The name, home and shell come from `/etc/passwd`
  when the file is there; a uid with no entry has no home, and gid defaults to
  the uid.
- `NAME`: `getent passwd` gives uid, gid, home and shell. Where `getent` is
  missing or does not know the name, isb reads `/etc/passwd` through the
  instance's file API (no program runs in the guest). A name in neither is
  the error `no such user NAME in INSTANCE`.
- `USER:GROUP`: `GROUP` is numeric or looked up with `getent group`, then
  `/etc/group` (`no such group GROUP in INSTANCE` when missing).

When the user resolves to a passwd entry, `HOME` (from the home), `USER` and
`LOGNAME` are set unless already given in the environment or per call. With a
TTY, `TERM` is set from the caller's `$TERM` (else `xterm-256color`) unless
given.

### `working_dir`

String, optional. Working directory for `command` and `isb exec SERVICE`.
Default: the user's home from passwd, if any. Per-call `isb exec -w` overrides
it. Client-side only.

### `exec`

More exec defaults, with no docker equivalent. Client-side only.

| Field | Type | Meaning |
|---|---|---|
| `env` | map or list of `KEY=VALUE` | Environment for exec only, over `environment`, never stored in the instance. Per-call `-e` wins over it. |
| `login` | bool | Run through the user's login shell. Default `false`. |

With `login`, argv becomes `SHELL -l -c 'exec "$@"' isb ARGV...`, so profile
scripts run while argv stays separate arguments. `SHELL` is the user's passwd
shell unless it ends in `nologin` or `/false`; otherwise, and when no user is
set, it is `/bin/sh`.

```yaml
user: dev
working_dir: /home/dev/src
exec:
  env: {PATH: "/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin"}
  login: false
```

### `command`

The sandbox's main command: argv (`[bun, run, dev]`), or a string split into
words the way a shell splits them (quotes group, backslash escapes) but never
run through one, as docker does: `bun run dev`.

Without `restart`, a foreground `isb up` runs it once the sandbox is ready, as
`user` in `working_dir` with the `exec` defaults, and streams its output
prefixed with `<service> | `. When every service's command has exited, `up`
stops the sandboxes and exits with the first non-zero status (0 if all
succeeded). Ignored by `isb up -d`. Client-side only, so changing it is never
drift.

With `restart` (and always under `isb stack deploy`), the command is supervised
inside the guest instead; see `restart`. On an OCI image it is the instance's
command line; see `image`.

```yaml
command: [sh, -c, "bun install && exec bun run dev"]
```

### `entrypoint`

OCI images only: argv or a string, like `command`. The command line is
`entrypoint` followed by `command`. An error on any other image.

### `restart`

`no` (default), `always`, `on-failure` or `unless-stopped`. Anything but `no`
makes the service long-running: it is meant to outlive `isb up`.

- The instance starts with the host: `always` and `on-failure` set
  `boot.autostart: true`; `unless-stopped` leaves it unset, which in incus
  restores whatever state the instance had at shutdown. All three set
  `boot.autorestart: true`, so incus restarts an instance whose init dies.
- On a system image, `command` is installed as a systemd unit,
  `isb-<service>.service`, with `Restart=always` (`on-failure` for
  `on-failure`) after 5 s, its environment (`environment` plus `exec.env`) in a
  0600 `/etc/isb/<service>.env`, `User=` and `WorkingDirectory=` from `user`
  and `working_dir`, and `exec.login` running it through the user's login
  shell. A program that is not an absolute path runs through `/bin/sh` so that
  `$PATH` applies. `isb up` rewrites the unit and restarts the app only when
  the unit or its environment changed. The image needs systemd; one without
  it is an error that says so.
- On an OCI image, incus restarts the app (`boot.autorestart`).
- A foreground `isb up` follows the app's output (the journal, or the console
  log) instead of running the command, and Ctrl-C stops the sandboxes as
  before. `isb logs SERVICE` shows recent output at any time.

```yaml
restart: always
command: [node, server.js]
```

### `healthcheck`

docker compose's healthcheck. `isb up` uses it for `depends_on` with
`condition: service_healthy`; `isb stack deploy` probes every replica on its
`interval`, keeps unhealthy ones out of the load balancer and restarts them.

Under `isb stack deploy` it is both of Kubernetes' probes. As a readiness
probe, throughout: a replica is in the load balancer only while it passes. As
a liveness probe, once it has passed since it (re)started: then `retries`
consecutive failures (after `start_period`) make it `unhealthy`, its app is
restarted, and after 3 such restarts in a row its instance is replaced. Until
its first pass a replica is `starting` (`isb stack ps`): out of the load
balancer, and not restarted for failing until its startup grace is over. The
grace is `start_period` when set, else `interval * retries * 2`, at least 60s
and at most 5m: 180s with the defaults, 60s with `interval: 5s`.
A rollout waits for a new replica through its grace, then `interval *
retries`, then 30s, before failing it.

| Field | Default | Meaning |
|---|---|---|
| `test` | required | `[CMD, argv...]`, `[CMD-SHELL, "shell line"]`, a plain string (a shell line), or `[NONE]`. Run in the guest as the service's `user`. |
| `interval` | `30s` | Between checks once healthy or unhealthy. |
| `timeout` | `30s` | One check's deadline. |
| `retries` | `3` | Consecutive failures before unhealthy. |
| `start_period` | none | After a start, failures do not count for this long. Unset, a replica that has not passed yet gets the startup grace above, and one that has passed counts every failure. |
| `start_interval` | `5s` | Between checks until the first result. |
| `disable` | `false` | Turn off a healthcheck set in another file. |

```yaml
healthcheck:
  test: [CMD, curl, -fsS, http://127.0.0.1:8080/health]
  interval: 10s
  start_period: 20s
```

### `depends_on`

Services to bring up first: a list of names, or a map to
`{condition: service_started | service_healthy}` (default `service_started`).
`isb up SERVICE` also brings up what it depends on, and `down` removes
dependents first. A cycle, an unknown service or a service depending on itself
is an error at load.

Under `isb up`, `service_healthy` probes the dependency's `healthcheck` until it
passes before touching its dependents. The dependency needs a `healthcheck`,
and its `command` must be supervised (`restart`), since a plain command only
starts once every service is up. Under `isb stack deploy`, a service waits until
its dependencies have a running (or healthy) replica.

```yaml
depends_on:
  db: {condition: service_healthy}
```

### `deploy`

For `isb stack deploy` (see [Stacks](../concepts/stacks.md)). `isb up` accepts it,
refuses `replicas` above 1, and applies only `resources`.

| Field | Default | Meaning |
|---|---|---|
| `mode` | `replicated` | The only mode. |
| `replicas` | `1` | Instances of the service. |
| `update_config` | | How a changed service rolls out: `parallelism` (1; 0 = all), `delay` (0s), `order` (`stop-first` or `start-first`), `monitor` (5s), `failure_action` (`pause`, `rollback` or `continue`). |
| `rollback_config` | | Accepted for compatibility; rollbacks use `update_config`. |
| `restart_policy` | | `condition` (`any`, `on-failure`, `none`), `delay` (5s), `max_attempts`, `window`. |
| `resources.limits` | | `cpus` (whole CPUs) and `memory`: the same as `cpus` and `mem_limit`, which they may not repeat. |
| `labels` | | Labels for the service's instances, merged over `labels`. |

### `domains`

Public hostnames `isb serve`'s ingress routes to the service's healthy
replicas, with certificates (see [Domains and ingress](../guides/domains.md)). Stacks only; `isb
up` ignores them. Changing them never replaces an instance.

| Field | Default | Meaning |
|---|---|---|
| `host` | (required) | `app.example.com`; `*.example.com` where the org allows wildcards; `auto` for a generated `<service>-<stack>-<org>.<ip>.sslip.io` name. |
| `path` | `/` | Path prefix: `/api` matches `/api` and `/api/...`. |
| `port` | (required without `redirect`) | The port the service listens on in its replicas. |
| `https` | `true` | Serve over HTTPS with a certificate the ingress obtains, redirecting HTTP to it. `false`: plain HTTP. |
| `redirect` | | Answer with a 308 to this URL instead of proxying; a URL without a path keeps the request's path and query. |
| `strip_prefix` | `false` | Remove `path` before passing the request on. |
| `www_redirect` | `false` | Also serve `www.<host>`, redirecting to `host`. |

```yaml
domains:
  - {host: shop.example.com, port: 8080}
  - {host: shop.example.com, path: /api, port: 3000, strip_prefix: true}
  - {host: auto, port: 8080}
```

### `secrets`

Secrets (top-level `secrets`) to write into the guest: names, or the long form
`{source, target, uid, gid, mode, on_change}`. Each becomes a file at
`/run/secrets/<target>` (default target: the source; an absolute target is used
as is), owned by `uid`/`gid` (default: a numeric `user`, else root) with `mode`
(default `0400`; YAML's unquoted `0400` and `"0400"` both mean octal).

`/run` is a tmpfs in a systemd guest, so isb also keeps a root-only copy in
`/var/lib/isb/secrets` with a script that puts the files back; a supervised
unit runs it before every start, so after a reboot the app has its secrets with
no isb around. On an OCI image the app is the instance's init, so isb writes
the files into a new instance before its first start: the app finds them as it
starts. An OCI instance already running without them gets them, and isb
restarts its app once so it reads them; files already in place (a daemon restart) restart nothing. The values never appear in instance
config or in `isb config`.
A new version of a secret reaches a stack's replicas per its
[`on_change`](#on_change): by default it is part of the revision, and a
rolling update replaces them.

```yaml
secrets:
  - db_password
  - {source: tls_key, target: tls.key, uid: 1000, mode: "0440"}
```

### `egress`

Confines the sandbox's network to a list of hostnames, and gives it secrets it
never holds ([Sandbox egress and secrets](../guides/egress.md)). Omitted, the
network is open.

```yaml
egress: none                          # no network at all
egress: [registry.npmjs.org, "*.example.com:8443"]
egress:
  allow: [db.example.com:5432]
  secrets:
    - API_TOKEN=acme-api-token@api.example.com        # ENV[=SECRET]@host1,host2
    - {env: OTHER, secret: other-token, hosts: [api.other.example:8443]}
```

- An entry is `host[:port]` (port 443 by default); `*.example.com` covers every
  name below `example.com`, not `example.com` itself. A host is a name, never
  an IP address.
- A secret's `env` is the variable the guest sees, holding a placeholder;
  `secret` names the org [secret](../guides/secrets.md) (default: `env`);
  `hosts` are where the real value may be sent, and are allowed too.
- `none` cannot be combined with hosts or secrets; an empty list means `none`.
- The network belongs to the sandbox from its creation: recreate it to add or
  remove `egress`. Editing the list or the secrets later is live. Needs
  `isb serve` for its proxy.

### `raw_config`

Map of string to string, set verbatim as instance config. Applied after every
typed field, so it overrides them (`raw_config: {limits.cpu: "2"}` beats
`cpus`). Reconciled like any config key; keys starting with `raw.` or
`security.` take effect on restart.

### `raw_devices`

Map of device name to a map of string properties, set verbatim. Each needs a
`type`. A name that collides with another device of this sandbox is an error.
Reconciled per the device rules.

`root` is special: its properties are merged over the generated root disk
(`{type: disk, path: /, pool: <storage pool>}`), for example to set `size`, and
need no `type`. `root: {size: ...}` alone is allowed for remote callers without
`--allow-raw`: it is how a stack service sizes its root disk against an org's
disk limit ([Orgs](../concepts/orgs.md#limits-are-budgets)). Like the rest of the root disk, they are used only at creation
and never reconciled.

```yaml
raw_devices:
  gpu: {type: gpu, gid: 44}
  root: {size: 20GiB}
```

## How reconcile works

`isb plan` and `isb up` resolve each sandbox against the host (pools, subids,
path map), read the instance, and diff. Plan lines: `+` create or add, `~`
change, replace or chown, `-` remove, `>` start, `note:` informational.
`isb plan --exit-code` exits 2 if any sandbox has a change (notes do not count).

**Creating.** Missing named volumes are created first (an `external` one that is
missing is an error). The instance is then created in one request with all
config and all devices except searched ports, so every mount, label and idmap
exists before first boot. Then it is started, searched ports are added, and
`owner` fixups run.

**Config.** Each key the file produces (`limits.*`, `security.privileged`,
`raw.idmap`, `user.*` from `labels`, `environment.*`, `raw_config`) is set if its value
differs. Keys not in the file, including ones incus sets, are never removed. A
changed `raw.*` or `security.*` key is reported as taking effect after
`isb restart NAME`; isb does not restart on its own. Other keys take effect as
incus applies them.

**Devices.** Only instance-local devices are considered, never ones inherited
from profiles. `root` is never reconciled. For each device in the file:

1. Present under its name and equal: never touched. Re-adding a disk remounts it,
   which silently kills inotify watches a running dev server holds. Equality
   ignores `readonly`, `shift` and `nat` set to `"false"` (same as absent) and a
   trailing `/` on a disk's `source` and `path`; a searched port is equal
   anywhere in its range.
2. Present under its name but different: replaced (a remount for a disk).
3. Absent, but an equal device exists under another name that the file does not
   use: adopted. It is left as is under its old name, with a note.
4. Absent, and a disk the file does not name is mounted at the same guest path:
   that disk is replaced by this one under the file's name, since two disks
   cannot share a mount point.
5. Otherwise added. A searched port is added after the start.

A searched port that is present under its name but not equal (step 2) is not
replaced in place: it is removed, and searched for again after the start.

A device the file does not mention is left alone, unless `--prune-devices` is
given; then it is removed, except `root` and any device adopted in step 3.

**Fixed at creation.** `type`, `storage` (the root pool) and `incus_profiles` are
never changed; a mismatch is reported as a note. `image` is never changed either;
a local alias that has moved to a new fingerprint since the instance was built is
reported as a note. Named volume `config`, `raw_devices.root` and
`container_name` are only used at creation and are not compared.

**Start.** A stopped instance is started after config and device changes are
written, so it boots with the right devices.

**Summary of `isb up` on an existing instance.** It will: create missing named
volumes, set differing config keys, replace wrong devices, add missing ones,
start a stopped instance, add searched ports, chown newly attached owned mounts,
and run readiness checks (skip them with `--no-ready`). It will not: recreate
the instance, change its type, root pool, incus profiles or image, unset config keys,
remove `raw.idmap`, restart the instance, touch a correct device, or remove any
device without `--prune-devices`.

### The ensure flow

For each selected service, `isb up`:

1. Takes an flock on `$XDG_RUNTIME_DIR/isb/<incus project>/<name>.lock` (else
   `<tmpdir>/isb-<uid>/isb/...`), waiting up to 15 minutes for another isb. The
   lock fd is close-on-exec, so it never leaks into exec'd children.
2. Plans: reads the instance, checks that a local image exists if it must
   create, checks which named volumes exist.
3. Applies. Config and device changes are batched into one read-modify-write
   update guarded by `If-Match`; a concurrent change by another tool causes a
   re-read and retry (up to 5 attempts), never a silent overwrite. A step that
   times out is retried once.
4. Waits for readiness.

At creation isb writes a random token to `user.isb.create-token`. If the create
times out, isb lets the operation settle, and deletes a half-created instance
only if it carries this call's token; an instance created by anyone else is left
alone. The key stays in the instance config afterwards but is not listed among
its labels. The create deadline is 10 minutes by default (`--create-timeout`).

Deleting (`isb rm -f`, `isb down`, `isb prune -y`, cleanup after a failed create)
force-stops a running instance, then retries the delete every 2 s for up to 60 s
while incus refuses it (an instance mid-transition, such as rebooting).

## Containers vs virtual machines

`type: container` (default) is a system container: it shares the host kernel,
starts quickly, and supports idmapped bind mounts and proxies in both
directions. `type: virtual-machine` (or `vm`, or `isb create --vm`) is a qemu VM
with its own kernel: a stronger boundary, at the cost of boot time and memory.
For a VM:

- `image` must be a VM image.
- `privileged` is an error.
- Host bind mounts are translated to the user running isb, whatever `idmap`
  you write: see [Host directories in a VM](#host-directories-in-a-vm).
- `ports` must be host-bound; `bind: guest` is an error. Each proxy gets
  `nat: "true"` automatically, since incus proxies into a VM only in NAT mode.
  With incus 7.0.1 or later, the default guest address `0.0.0.0` lets incus
  find the VM's address itself. Older incus needs a static IP on the VM's NIC
  (`raw_devices: {eth0: {type: nic, network: incusbr0, ipv4.address: ...}}`)
  and that address in `connect`. A NAT listen on host `127.0.0.1` does not
  work (`route_localnet` is off on the bridge), so publish on another address
  (`"100.64.0.5:8080:80"`).

- NAT forwarding is DNAT and does not pass through a host firewall such as
  ufw. Listen on the specific address you mean to expose (a tailnet IP, say),
  never `0.0.0.0` on a host with a public interface.
- Host bind mounts are shared over virtiofs, where inotify events for host-side
  edits are not delivered. File watchers inside the VM (dev servers, test
  watchers) need polling.
- The default `ready` is `[running, agent]`: exec goes through the incus agent,
  which starts some time after the VM does. `owner` fixups wait for the agent.
- A guest-initiated reboot during first boot is handled as described under
  `ready`.

`isb port add` on an existing VM applies the same rules.

## CLI shorthands

Used by `isb create` and `isb port add`.

**`-v SRC:GUEST[:OPTS]`** A mount, in the compose short syntax. `SRC` starting
with `/`, `.` or `~` is a bind path; anything else is a named volume, used as
the incus volume name. `GUEST` must be absolute and cannot contain `:`.

```
-v ./src:/home/dev/src:ro,device=src
-v dev-cache:/home/dev/.cache:owner=dev
```

**`-p [IP:]PUBLISHED:TARGET[/tcp|/udp]`** Publish a guest port, in the compose
short syntax, ranges included.

**`-p listen=ADDR,connect=ADDR[,bind=host|guest][,name=N][,search=N][,OPT=V...]`**
The full form. `OPT` is one of the proxy options `nat`, `proxy_protocol`, `uid`,
`gid`, `mode`, `security.uid`, `security.gid`; any other key is an error. Values
cannot contain commas.

**`--ready CHECK`** (repeatable) `running`, `agent`, `default_route`,
`user_exists=USER`, `path_writable=PATH`, `command=ARG[,ARG...]`.

## CLI verbs

Global flags: `--socket` (`$INCUS_SOCKET`), `--project` (`$INCUS_PROJECT`),
`--org` (`$ISB_ORG`), `-f/--file`, `--env-file`, `-P/--project-name`,
`--create-timeout`, `-q/--quiet`. The [CLI reference](cli.md) lists every
command.

Compose (take service names; all services when none are given):

| Verb | Does |
|---|---|
| `up [SVC...] [-d] [--no-log-prefix] [-t D] [--prune-devices] [--no-ready] [--json]` | Create or reconcile. For every published range, prints `SERVICE DEVICE LISTEN` on stdout with the listen address in use, whether `up` added the device or found it already correct. With `--json`, each report's `ports` object maps device to that address. Then, like `docker compose up`, stays in the foreground (see [Foreground `up`](#foreground-up)); `-d` returns instead and leaves the sandboxes running. |
| `plan [SVC...] [--prune-devices] [--json] [--exit-code]` | Show what `up` would change. |
| `down [SVC...] [--volumes]` | Delete the sandboxes (running ones are stopped). With `--volumes` and no service list, also delete the file's non-external named volumes: every one a service mounts, in the pool `up` used (mount `pool`, else top-level `pool`, else that sandbox's `storage` pool), plus declared ones no service mounts (in their `pool`, `auto` meaning the host default). A volume still in use is kept with a message. With a service list, `--volumes` is ignored with a message. |
| `config [--services]` | Print the resolved file. |
| `ps [SVC...] [--json]` | Status per service; without a compose file, running instances. |
| `exec TARGET [-u USER] [-w CWD] [-e K=V] [-l] [-t\|-T] [-i\|-n] [--timeout D] -- ARGV...` | Run argv (no shell). `TARGET` is a service (by key or by its instance name) if a compose file defines it, else an instance. TTY when stdin is a terminal; stdin is forwarded for a terminal, `-T` or `-i`, else the command sees EOF. Exit code is the command's, or 125 if isb itself failed. |
| `inspect NAME [--json]` | One sandbox, by service or instance name. |
| `logs SERVICE [-n 100]` | Recent output of a long-running (`restart`) or OCI service: its unit's journal, or the console log. |

### Foreground `up`

Without `-d`, `up` holds the sandboxes after creating or reconciling them. It
runs each service's `command` and streams its output (`--no-log-prefix` drops
the `<service> | ` prefix), then stops the selected sandboxes when the first of
these happens:

| Event | Exit code |
|---|---|
| every `command` has exited (never, when no service has one) | first non-zero status, else 0 |
| SIGINT, SIGTERM or SIGHUP | 128 + signal (130 for Ctrl-C) |
| a process that started isb exits | 129 |
| stdout is closed (the reader of the pipe went away) | 141 |

The third is the one signals cannot give. A closed terminal, or an agent whose
background task ends with the agent, does not always signal its descendants,
which would leave a sandbox and its published ports running with nobody
attached. isb records its ancestors (below pid 1) at startup and checks them
every second, so it goes when whatever started it goes. That includes a
wrapper script that backgrounds `isb up` and exits: use `-d` for that.

Stopping is a clean shutdown bounded by `-t` (default `10s`), then a kill; a
second Ctrl-C kills at once. Sandboxes are stopped, not deleted, as with
`docker compose up`: the next `up` starts them again with their state intact,
and `isb down` deletes them.

Instances (take instance names):

| Verb | Does |
|---|---|
| `create NAME -i IMAGE [flags] [--ensure] [--no-ready]` | Create from flags (`--cpus`, `--cpuset-cpus`, `-m`, `-s`, `--idmap`, `--privileged true\|false`, `-l`, `-e`, `-v`, `-p`, `--ready`, `--ready-timeout`, `-c k=v`, `--profile`, `--vm`). Fails if it exists unless `--ensure`, which reconciles like `up`. `--no-ready` skips readiness in both modes. Relative bind paths resolve against the current directory. `--idmap` also accepts a raw value. |
| `start`, `stop [-f] [-t 30s]`, `restart` | Lifecycle. `start` and `restart` wait for `running` only, not the file's `ready` checks. |
| `rm [-f] NAME...` | Delete (`-f` stops a running one first). Aliases `remove`, `delete`. |
| `ls [-l KEY[=VALUE]...] [--json]` | List, filtered by labels (`user.isb.*` keys are not labels). |
| `port add NAME SPEC [--name N] [--search N]`, `port rm NAME DEV...`, `port get NAME DEV [KEY]`, `port ls NAME [--json]` | Proxy devices on an existing sandbox. `add` leaves a correct device alone and prints the listen address. `get` prints one property (default `listen`) as plain text, and fails if the device or property is missing. `ls --json` prints an object keyed by device name, each value the device's properties as strings: `{"vite": {"type": "proxy", "bind": "host", "listen": "tcp:100.1.2.3:5176", "connect": "tcp:127.0.0.1:5173"}}`. On a VM the VM rules apply: `nat: "true"` is added and `bind=guest` is refused. |
| `device ls NAME`, `device rm NAME DEV...` | Instance-local devices (`root` cannot be removed). |
| `volume create\|ls\|inspect\|rm` | Named volumes (`--pool`, `-c k=v`). `rm` is refused while in use. |
| `prune --label KEY --missing-path [-y] [--json]` | Delete instances whose `KEY` label is an absolute host path that no longer exists. Dry run without `-y`. |
| `schema` | Print the JSON Schema. |

## Annotated example

```yaml
name: webapp                      # sandboxes webapp-<service>, volumes webapp_<key>
incus_project: default

x-base: &base                     # dropped after anchors are resolved
  image: images:debian/12
  storage: auto                   # incus-zfs, else default, else first pool
  incus_profiles: [default]
  idmap: auto                     # raw.idmap only where /etc/subuid needs it

volumes:
  bun-cache: {}                   # webapp_bun-cache, created on first use
  datasets:
    external: true                # must already exist, named exactly datasets
    pool: default

services:
  web:
    <<: *base
    container_name: "${WEB_NAME:-webapp-web}"
    cpus: "${CPUS:-8}"            # limits.cpu
    mem_limit: 8g                 # limits.memory 8GiB
    privileged: false             # security.privileged (restart to apply)
    labels:
      worktree: "${WORKTREE:?set WORKTREE}"   # user.worktree, for isb prune
    environment:
      - NODE_ENV=development      # environment.NODE_ENV; never a secret
    volumes:
      - ./src:/home/dev/src:device=src        # relative to this file
      - type: volume
        source: bun-cache
        target: /home/dev/.bun/install/cache
        owner: dev                # chown after attach, plus root-owned parents in ~dev
      - datasets:/data:ro
    ports:
      - "${LISTEN_IP:-127.0.0.1}:5173-5223:5173"  # first free of 5173..5223
      - name: backend             # guest listens, host connects
        bind: guest
        listen: tcp:127.0.0.1:8190
        connect: "tcp:127.0.0.1:${BACKEND_PORT:-8080}"
    ready:
      - running
      - default_route
      - user_exists: dev
      - path_writable: /home/dev/src   # as user
    ready_timeout: 90s
    user: dev                     # HOME, USER, LOGNAME from passwd
    working_dir: /home/dev/src
    exec:
      env:
        PATH: "/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin"
    command: bun run dev          # run by a foreground isb up
    raw_config:
      security.nesting: true      # stored as "true"; restart to apply
    raw_devices:
      scratch: {type: disk, source: /srv/scratch, path: /scratch}
```

Then:

```
isb config                 # check interpolation and merging
isb plan                   # see what would change
isb up -d                  # create or reconcile, wait for readiness, return
isb exec web -- bun install
```
