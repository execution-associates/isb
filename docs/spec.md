# isb compose file reference

A compose file describes named storage volumes and any number of sandboxes (incus
containers or VMs). `isb up` creates what is missing and reconciles what exists,
changing only what differs. isb talks to incusd over its unix socket
(`--socket`, else `$INCUS_SOCKET`, else `$INCUS_DIR/unix.socket`, else
`/var/lib/incus/unix.socket`).

## Files and validation

- With no `-f`, isb looks in the current directory for `isb.yaml`, then `isb.yml`.
- `-f FILE` may be repeated. It is accepted before the subcommand and after the
  compose-aware ones (`up`, `down`, `plan`, `config`, `ps`, `inspect`, `exec`):
  `isb -f a.yaml up -f b.yaml` loads `a.yaml` then `b.yaml`.
- Files are merged in order. Mappings merge key by key, recursively. Everything
  else (scalars and sequences such as `ports`, `ready`, `profiles`) is replaced
  by the later file. `volumes` merges per guest path and then field by field.
- Each file is also validated on its own, so an error names the file.
- Relative bind paths resolve against the directory of the first file, including
  mounts declared in later files.
- Every object rejects unknown fields, so a typo is an error. Keys starting with
  `x-` are dropped at the top level and directly inside each sandbox (and only
  there). They are useful as YAML anchor holders. YAML anchors, aliases and
  `<<` merge keys work within one file.

To check a file without touching anything:

- `isb config` prints the resolved file (interpolated, merged, project name and
  sandbox names filled in). `isb config --services` prints the service names.
- `isb plan` shows what `up` would change (see "How reconcile works").
- `isb schema` prints the JSON Schema of the format.

### Value types

Interpolation produces strings, so typed fields also accept strings:

- Booleans (`privileged`, `readonly`, `external`, `login`) accept `true`/`false`
  or a string: `true`, `yes`, `on`, `1`, `false`, `no`, `off`, `0` or empty,
  case-insensitive.
- `cpus`, `memory`, `ready_timeout`, `exec.user` and a mount's `owner` accept a
  non-negative integer or a string. Floats are rejected.
- `search` accepts an integer or a numeric string, at most 65535.
- Durations (`ready_timeout`): a number with an optional unit `ms`, `s`/`sec`/
  `secs`, `m`/`min`/`mins`, `h`. No unit means seconds. Decimals work (`1.5m`).
- The values of free-form string maps (`labels`, `env`, `raw_config`, the
  properties in `raw_devices`, mount and port `options`, top-level volume
  `config`, `exec.env`) may be any scalar and are stored as its string form:
  `env: {DEBUG: 1}` is `"1"`, `raw_config: {security.nesting: true}` is
  `"true"`. A float is written in its shortest form (`1.0` becomes `"1"`), so
  quote a value whose exact spelling matters. Map keys must be strings.
- Other string fields (`name`, `image`, `storage`, `exec.cwd`, ...) take YAML
  strings only.

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

Variable sources: the process environment, then `--env-file FILE` (repeatable).
The process environment wins; a later env file overrides an earlier one. An env
file has `KEY=VALUE` lines, blank lines and `#` comment lines are skipped, an
`export ` prefix is allowed, one pair of matching `"` or `'` around the value is
removed, and nothing is expanded. Inline comments are not stripped. A non-blank
line without `=` is an error.

## Top-level fields

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | directory of the first file | Compose project name. |
| `project` | string | `default` | incus project to operate in. |
| `volumes` | map of name to volume | `{}` | Named custom storage volumes. |
| `sandboxes` | map of service to sandbox | `{}` | The sandboxes, keyed by service name. |

`name` is sanitized: lowercased, each run of characters outside `[a-z0-9]`
becomes one `-`, leading and trailing `-` are trimmed, and `isb-` is prepended if
the result does not start with a letter (an empty result becomes `isb`).
`-P/--project-name` overrides it. The default sandbox name is
`<name>-<service>`, with the service key sanitized the same way (`Web_1` in
project `lasso` becomes `lasso-web-1`).

`project` is overridden by `--project` or `$INCUS_PROJECT`. It also scopes the
per-sandbox lock (see "The ensure flow").

## `volumes.<name>`

A named custom storage volume (filesystem content type). Declaring it here is
optional: a volume a sandbox mounts but that is not declared is created with no
config. A declared volume that no sandbox mounts is never created.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `pool` | string | `auto` | Storage pool. `auto` means the root pool of the sandbox that mounts it. A named pool must exist. |
| `config` | map of string | `{}` | Volume config keys (`size: 10GiB`). Applied only when isb creates the volume; never compared or changed later. |
| `external` | bool | `false` | The volume must already exist. isb never creates it; `plan` and `up` fail if it is missing. |

A volume is created before the sandbox that mounts it. If two sandboxes resolve
the same volume to different pools, each pool gets its own volume.

## `sandboxes.<service>`

Each field below lists its type, default, what it becomes in incus, and whether
`isb up` reconciles it on an existing instance or only uses it at creation.
"Reconciled" for config keys means the key is set when its value differs. isb
never unsets a key: removing a field, label or env var from the file leaves the
old key on the instance.

### `name`

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

Integer or string, optional. Becomes `limits.cpu`: a count (`8`) or a CPU set
(`0-3`). Reconciled, live.

### `memory`

Integer or string, optional. Becomes `limits.memory` (`8GiB`). Reconciled, live.

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
range. uid and gid are decided separately.

The value is `both H G` when uid and gid need mapping with the same pair,
otherwise one line per id that needs it: `uid HOST GUEST` and/or
`gid HOST GUEST`, joined by a newline.

Reconciled as `raw.idmap`; takes effect on restart. isb never removes the key.
If the instance has a `raw.idmap` that the spec does not produce, `plan` notes it
("not needed on this host" for `auto`, "the spec says idmap: none" for `none`)
and leaves it to be unset by hand.

Container-only. On a VM, `auto` and `none` are accepted and do nothing; any
other form (`always`, the map form including `{}`, or `{raw: ...}`) is an error.

### `profiles`

List of strings, default `[default]`. Applied in order at creation. Fixed at
creation; a different list (order counts) is reported as a note.

### `labels`

Map of string to string. Each becomes config key `user.<key>`. A key must be
non-empty and contain no whitespace. Reconciled, live. Labels not in the file are
never removed. Used by `isb ls --label key[=value]` and
`isb prune --label key --missing-path`. isb itself writes
`user.isb.create-token` at creation (see below); `user.isb.*` keys are
bookkeeping and are not shown as labels.

```yaml
labels: {app: web, worktree: "${WORKTREE}"}
```

### `env`

Map of string to string. Each becomes `environment.<KEY>`, which incus applies to
every exec. Reconciled. This is plain instance config, readable by anyone who can
read the instance: never put a secret here.

### `volumes`

Map keyed by the absolute path inside the guest. A trailing `/` is ignored; the
same path twice (after that) is an error. Each entry sets exactly one of `bind`
or `named`.

| Field | Applies to | Type | Default | Meaning |
|---|---|---|---|---|
| `bind` | bind | string | | Host path to bind-mount. |
| `named` | named | string | | Named custom volume to mount. |
| `pool` | named | string | see below | Pool of the named volume. |
| `readonly` | both | bool | `false` | Mount read-only (`readonly: "true"`). |
| `external` | named | bool | `false` | The volume must already exist. |
| `owner` | named | string or int | | chown the mount point in the guest. |
| `device` | both | string | derived | Device name. |
| `options` | both | map of string | `{}` | Extra disk device properties (`shift`, `propagation`, ...), verbatim. |

`owner`, `pool` and `external` on a bind mount are errors (isb never chowns host
paths).

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

**Named volume pool.** The mount's `pool`, else the top-level volume's `pool`,
else `auto`. `auto` means this sandbox's root pool (`storage`). A named pool must
exist.

**`external`.** True on either the mount or the top-level volume makes the
volume required: `plan` and `up` fail if it does not exist.

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
  /home/dev/src: {bind: ./src, device: src}
  /home/dev/.cache: {named: dev-cache, owner: dev}
  /srv/ref: {bind: ~/ref, readonly: true, options: {shift: "true"}}
```

### `ports`

List of proxy devices, in either direction.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | derived | Device name. |
| `bind` | `host` or `guest` | `host` | `host`: listen on the host, connect in the guest (publish a guest port). `guest`: listen in the guest, connect on the host (reach a host service). |
| `listen` | string | required | An address (see below). |
| `connect` | string | required | An address (see below). |
| `search` | integer | none | Host-bound TCP/UDP only: step past a taken listen port, up to this many more. |
| `options` | map of string | `{}` | Extra proxy properties (`nat`, `proxy_protocol`, ...), verbatim. |

**Addresses.** Docker-style shorthand works; the protocol defaults to `tcp` and
the host to `127.0.0.1`:

| Written | Means |
|---|---|
| `5173` | `tcp:127.0.0.1:5173` |
| `0.0.0.0:5173` | `tcp:0.0.0.0:5173` |
| `5353/udp` | `udp:127.0.0.1:5353` |
| `tcp:5173`, `udp:5353` | `tcp:127.0.0.1:5173`, `udp:127.0.0.1:5353` |
| `tcp:HOST:PORT`, `udp:HOST:PORT`, `unix:PATH` | as written |

On a VM, `connect` defaults to host `0.0.0.0` instead, which lets incus' NAT mode
find the VM's address. IPv6 hosts go in brackets (`[::1]:8080`). The port may be
a range or a list, as incus allows (`8000-8010`, `80,443`). isb stores the full
form, so `5173` in the spec matches an existing device written as
`tcp:127.0.0.1:5173`, and reconcile leaves it alone. Becomes
`{type: proxy, bind, listen, connect, ...options}`; on a VM, `nat: "true"` is
added before `options`. An option named `type`, `bind`, `listen` or `connect` is
an error ("would override a core property"). Reconciled per the device rules. On
a VM, `bind: guest` is an error.

**Default name.** `port-<bind>-<port>` for TCP (`port-host-5173`),
`port-<bind>-udp-<port>` for UDP, and `port-<bind>-<derived>` for unix sockets,
where `<derived>` is the device-name rule applied to the whole address
(`unix:/run/app.sock` gives `port-guest-unix-run-app-sock`).

**`search`.** Set with `bind: guest` or a `unix:` listen address, it is an error
(also when it is `0`); `0` otherwise means no search. A searched port is not in
the create request. It is added after the instance is running: for each port from
the requested one to requested + `search`, isb first tries to bind it locally
(TCP only) and skips it if it is in use, then adds the device, and moves to the
next port if incus refuses. `isb up` prints `<service> <device> <listen>` for
each port it added this way. An existing device with that name whose listen port
is anywhere in the range (same protocol and address, every other property equal)
counts as correct. One that is not (outside the range, or otherwise different) is
removed and searched for again after the start, rather than replaced at a port
that may be taken.

```yaml
ports:
  - {name: vite, listen: "${IP}:5173", connect: 5173, search: 50}
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
| `{path_writable: PATH}` | `test -w PATH` exits 0, run as `exec.user` (else root). Only the user is taken from `exec`; its `cwd` defaults to that user's home. |
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

### `exec`

Defaults for `isb exec SERVICE` (when the target is a service name resolved
through the compose file) and for the `path_writable` check. Per-call flags
override them. Client-side only; nothing in incus changes.

| Field | Type | Meaning |
|---|---|---|
| `user` | string or int | `NAME`, `UID`, `UID:GID` or `NAME:GROUP`. Default root. |
| `cwd` | string | Working directory. Default: the user's home from passwd, if any. |
| `env` | map of string | Exec environment, over the instance `env`. Per-call `-e` wins over it. |
| `login` | bool | Run through the user's login shell. Default `false`. |

User resolution, in the guest, as root:

- `NAME` or `UID`: `getent passwd` gives uid, gid, home and shell. A numeric
  value with no passwd entry becomes uid = gid = that number, with no home.
  Anything else missing is an error.
- `UID:GID` (both numeric): those ids; name, home and shell come from
  `getent passwd UID` when it exists.
- `NAME:GROUP`: `NAME` must exist; `GROUP` is numeric or looked up with
  `getent group`.

When the user resolves to a passwd entry, `HOME` (from the home), `USER` and
`LOGNAME` are set unless already given in `env` or per call. With a TTY, `TERM`
is set from the caller's `$TERM` (else `xterm-256color`) unless given.

With `login`, argv becomes `SHELL -l -c 'exec "$@"' isb ARGV...`, so profile
scripts run while argv stays separate arguments. `SHELL` is the user's passwd
shell unless it ends in `nologin` or `/false`; otherwise, and when no user is
set, it is `/bin/sh`.

```yaml
exec:
  user: dev
  cwd: /home/dev/src
  env: {PATH: "/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin"}
  login: false
```

### `command`

The sandbox's main command, as argv (no shell): `[bun, run, dev]`. A
foreground `isb up` runs it once the sandbox is ready, with the service's
`exec` defaults, and streams its output prefixed with `<service> | `. When
every service's command has exited, `up` stops the sandboxes and exits with the
first non-zero status (0 if all succeeded). Ignored by `isb up -d`. Client-side
only, so changing it is never drift.

```yaml
command: [sh, -c, "bun install && exec bun run dev"]
```

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
need no `type`. Like the rest of the root disk, they are used only at creation
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
`raw.idmap`, `user.*`, `environment.*`, `raw_config`) is set if its value
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

**Fixed at creation.** `type`, `storage` (the root pool) and `profiles` are
never changed; a mismatch is reported as a note. `image` is never changed either;
a local alias that has moved to a new fingerprint since the instance was built is
reported as a note. Named volume `config`, `raw_devices.root` and `name` are only
used at creation and are not compared.

**Start.** A stopped instance is started after config and device changes are
written, so it boots with the right devices.

**Summary of `isb up` on an existing instance.** It will: create missing named
volumes, set differing config keys, replace wrong devices, add missing ones,
start a stopped instance, add searched ports, chown newly attached owned mounts,
and run readiness checks (skip them with `--no-ready`). It will not: recreate
the instance, change its type, root pool, profiles or image, unset config keys,
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
- `idmap` other than `auto` or `none` is an error; `auto` is a no-op.
- `ports` must be `bind: host`; `bind: guest` is an error. Each proxy gets
  `nat: "true"` automatically, since incus proxies into a VM only in NAT mode.
  With incus 7.0.1 or later, `connect: tcp:0.0.0.0:PORT` lets incus find the
  VM's address itself. Older incus needs a static IP on the VM's NIC
  (`raw_devices: {eth0: {type: nic, network: incusbr0, ipv4.address: ...}}`)
  and that address in `connect`. A NAT listen on host `127.0.0.1` does not
  work (`route_localnet` is off on the bridge).

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

**`-v SRC:GUEST[:OPTS]`** A mount. `SRC` starting with `/`, `.` or `~` is a bind
path; anything else is a named volume. `GUEST` must be absolute and cannot
contain `:`. `OPTS` is a comma list of `ro`, `rw`, `external`, `owner=USER`,
`device=NAME`, `pool=POOL`.

```
-v ./src:/home/dev/src:ro,device=src
-v dev-cache:/home/dev/.cache:owner=dev
```

**`-p [IP:]HOSTPORT:GUESTPORT[/tcp|/udp]`** Publish a guest port: `bind: host`,
listen `PROTO:IP:HOSTPORT` (IP default `127.0.0.1`), connect
`PROTO:127.0.0.1:GUESTPORT`, protocol default `tcp`.

**`-p listen=ADDR,connect=ADDR[,bind=host|guest][,name=N][,search=N][,OPT=V...]`**
The full form. `OPT` is one of the proxy options `nat`, `proxy_protocol`, `uid`,
`gid`, `mode`, `security.uid`, `security.gid`; any other key is an error. Values
cannot contain commas.

**`--ready CHECK`** (repeatable) `running`, `agent`, `default_route`,
`user_exists=USER`, `path_writable=PATH`, `command=ARG[,ARG...]`.

## CLI verbs

Global flags: `--socket`, `--project` (`$INCUS_PROJECT`), `-f/--file`,
`--env-file`, `-P/--project-name`, `--create-timeout`, `-q/--quiet`.

Compose (take service names; all services when none are given):

| Verb | Does |
|---|---|
| `up [SVC...] [-d] [--no-log-prefix] [-t D] [--prune-devices] [--no-ready] [--json]` | Create or reconcile. For every port with `search`, prints `SERVICE DEVICE LISTEN` on stdout with the listen address in use, whether `up` added the device or found it already correct. With `--json`, each report's `ports` object maps device to that address. Then, like `docker compose up`, stays in the foreground (see [Foreground `up`](#foreground-up)); `-d` returns instead and leaves the sandboxes running. |
| `plan [SVC...] [--prune-devices] [--json] [--exit-code]` | Show what `up` would change. |
| `down [SVC...] [--volumes]` | Delete the sandboxes (running ones are stopped). With `--volumes` and no service list, also delete the file's non-external named volumes: every one a sandbox mounts, in the pool `up` used (mount `pool`, else top-level `pool`, else that sandbox's `storage` pool), plus declared top-level ones no sandbox mounts (in their `pool`, `auto` meaning the host default). A volume still in use is kept with a message. With a service list, `--volumes` is ignored with a message. |
| `config [--services]` | Print the resolved file. |
| `ps [SVC...] [--json]` | Status per service; without a compose file, running instances. |
| `exec TARGET [-u USER] [-w CWD] [-e K=V] [-l] [-t\|-T] [-n] [--timeout D] -- ARGV...` | Run argv (no shell). `TARGET` is a service if a compose file defines it, else an instance. TTY when stdin is a terminal. Exit code is the command's, or 125 if isb itself failed. |
| `inspect NAME [--json]` | One sandbox, by service or instance name. |

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
which used to leave a sandbox and its published ports running with nobody
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
| `create NAME -i IMAGE [flags] [--ensure] [--no-ready]` | Create from flags (`--cpus`, `-m`, `-s`, `--idmap`, `--privileged true\|false`, `-l`, `-e`, `-v`, `-p`, `--ready`, `--ready-timeout`, `-c k=v`, `--profile`, `--vm`). Fails if it exists unless `--ensure`, which reconciles like `up`. `--no-ready` skips readiness in both modes. Relative bind paths resolve against the current directory. `--idmap` also accepts a raw value. |
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
name: webapp                      # sandbox names default to webapp-<service>
project: default                  # incus project

x-base: &base                     # dropped after anchors are resolved
  image: images:debian/12
  storage: auto                   # incus-zfs, else default, else first pool
  profiles: [default]
  idmap: auto                     # raw.idmap only where /etc/subuid needs it

volumes:
  bun-cache: {}                   # created on first use, in the sandbox's pool
  datasets:
    external: true                # must already exist
    pool: default

sandboxes:
  web:
    <<: *base
    name: "${WEB_NAME:-webapp-web}"
    cpus: "${CPUS:-8}"            # limits.cpu
    memory: 8GiB                  # limits.memory
    privileged: false             # security.privileged (restart to apply)
    labels:
      worktree: "${WORKTREE:?set WORKTREE}"   # user.worktree, for isb prune
    env:
      NODE_ENV: development       # environment.NODE_ENV; never a secret
    volumes:
      /home/dev/src:
        bind: ./src               # relative to this file; symlinks resolved
        device: src
      /home/dev/.bun/install/cache:
        named: bun-cache
        owner: dev                # chown after attach, plus root-owned parents in ~dev
      /data:
        named: datasets
        readonly: true
    ports:
      - name: vite                # host listens, guest connects
        listen: "tcp:${LISTEN_IP:-127.0.0.1}:5173"
        connect: tcp:127.0.0.1:5173
        search: 50                # 5173..5223, first free wins
      - name: backend             # guest listens, host connects
        bind: guest
        listen: tcp:127.0.0.1:8190
        connect: "tcp:127.0.0.1:${BACKEND_PORT:-8080}"
    ready:
      - running
      - default_route
      - user_exists: dev
      - path_writable: /home/dev/src   # as exec.user
    ready_timeout: 90s
    exec:
      user: dev                   # HOME, USER, LOGNAME from passwd
      cwd: /home/dev/src
      env:
        PATH: "/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin"
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
