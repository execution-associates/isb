# The isb CLI

The `isb` CLI does what these tools do. On a host it talks to incus over its
socket and to `isb serve` over the daemon's unix socket; inside a workspace
its daemon commands use `$ISB_URL` and `$ISB_TOKEN`. `isb --help` and
`isb <command> --help` list everything. Install:
`curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh`
(signature checked; `isb update` keeps it current). Do not install it with
mise: that skips the signature check.

Only the CLI: `isb up`/`down`/`plan` of a whole compose file, `isb host setup`,
`isb machine` (macOS), `isb serve`, `isb tui`, `isb ssh-config` and
`isb ssh-proxy`, `isb update`, and the host trust roots
(`isb superadmin`, superadmin tokens, bind roots).

## isb.yaml: dev sandboxes on a host

Docker compose's format (`services`, `container_name`, `environment`,
`volumes`, `ports`, `user`, `working_dir`, `command`, `mem_limit`) plus
incus keys: `type: vm`, `storage`, `idmap`, `ready`, `incus_profiles`,
`raw_config`, `raw_devices`, `egress`.

```yaml
services:
  web:
    image: dev-base
    idmap: auto                        # files made inside stay yours
    labels: { owner: "${USER}" }
    volumes:
      - ./src:/home/dev/src            # relative to this file
    ports: ["5173-5223:5173"]          # host 127.0.0.1, first free port
    ready: [running, default_route, { user_exists: dev }]
    user: dev
    working_dir: /home/dev/src
    command: sh -c "bun install && exec bun run dev"
```

- `isb up` creates or reconciles, then **blocks** until every `command` exits,
  Ctrl-C, or its caller goes away, and then stops (not deletes) the
  sandboxes. A script or tool call that continues needs **`isb up -d`**. A
  dev server that should die with you runs as plain `isb up` in a background
  task, never `isb up &` in a wrapper that exits.
- `isb plan` (what would change), `isb ps`, `isb exec SVC -- argv`,
  `isb logs SVC`, `isb port get NAME DEV`, `isb config`, `isb down`
  (`--volumes` also deletes named volumes).
- `isb exec` exits with the command's status; **125 means isb failed**. Piping
  stdin in needs `-i`. `-u USER`, `-w DIR`, `-e K=V`, `--timeout 5m`.
- `${VAR}` comes from the environment, then `.env`; unset is an error (use
  `${VAR:-default}`). `isb.override.yaml` merges over `isb.yaml`.
- Ports need the host port and listen on 127.0.0.1 unless you write an
  address. Named volumes are `<project>_<key>`. The image, pool, profiles and
  type are fixed at creation: `isb down` then `isb up` to change them.
- One-off: `isb create task1 -i images:ubuntu/24.04 -v ./repo:/work -l owner=me`,
  `isb exec task1 -w /work -- make test`, `isb rm -f task1`.

## SDKs

Python `isb-sdk` and TypeScript `@execution-associates/isb`:
`Sandbox.connect_or_create(...)` / `connectOrCreate`, `exec`, `exec_stream`,
`remove`, and `Project.load("isb.yaml")` with `up()`, `plan()`, `down()`.
They drive the local `isb` binary, so they need the incus socket too.

Docs: https://github.com/execution-associates/isb/tree/main/docs
