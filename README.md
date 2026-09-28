# isb

Declarative incus sandboxes: a Rust library, a CLI, and a compose-style YAML
format. isb talks to incusd over its unix socket, never through the `incus`
binary.

```yaml
# isb.yaml
volumes:
  bun-cache: {}
sandboxes:
  web:
    image: dev-base
    cpus: 8
    memory: 8GiB
    idmap: auto
    labels: { app.worktree: "${WORKTREE}" }
    volumes:
      /home/dev/src: { bind: ./src, device: src }
      /home/dev/.bun/install/cache: { named: bun-cache, owner: dev }
    ports:
      - { name: vite, bind: host, listen: "tcp:${IP}:5173", connect: "tcp:127.0.0.1:5173", search: 50 }
    ready: [running, default_route, { user_exists: dev }]
    exec: { user: dev, cwd: /home/dev/src }

  # The same, as a virtual machine: its own kernel instead of the host's.
  worker:
    type: vm
    image: images:debian/13/cloud
    cpus: 4
    memory: 4GiB
    volumes:
      /srv/data: { bind: ./data }
    ready: [running, agent, default_route]
```

```console
$ isb plan                  # what would change
$ isb up                    # create, or change only what differs
$ isb exec web -- bun install
$ isb down
```

## Install

isb is not on crates.io yet.

```sh
# From git
cargo install --git https://github.com/execution-associates/isb

# From a checkout
cargo install --path .

# With mise (git source until it is published)
mise use -g "cargo:https://github.com/execution-associates/isb@branch:main"
# After publishing to crates.io:
mise use -g cargo:isb
```

Building runs build scripts and proc macros from dependencies. Build inside a
sandbox if that matters to you (see [Development](#development)).

isb needs access to the incus socket (`$INCUS_SOCKET`, else
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`), which usually
means membership in `incus-admin`. That access is root-equivalent on the host.

## CLI

```text
isb create NAME -i IMAGE [--vm] [--cpus N] [-m MEM] [-v SRC:GUEST[:ro,owner=U]] [-p [IP:]HOST:GUEST]
                         [-l k=v] [-e K=V] [--idmap auto] [--ready CHECK] [--ensure]
isb start|stop|restart|rm NAME...
isb ls [--label k[=v]] [--json]            list, filtered by label
isb inspect NAME [--json]
isb exec NAME|SERVICE [-u USER] [-w DIR] [-e K=V] [-l] [-t|-T] [-n] [--timeout D] -- ARGV...
isb volume create|ls|inspect|rm
isb port add NAME SPEC [--name DEV] [--search N]   prints the listen address in use
isb port get NAME DEV [KEY]                prints one property, default: listen
isb port rm NAME DEV... | isb port ls NAME [--json]
isb device ls|rm NAME ...
isb prune --label KEY --missing-path [-y]  delete sandboxes whose label is a vanished host path
isb schema                                 JSON Schema of the YAML format

# compose (-f FILE, repeatable; default ./isb.yaml)
isb up [SERVICE...] [--prune-devices] [--no-ready] [--json]
isb plan [SERVICE...] [--json] [--exit-code]
isb down [SERVICE...] [--volumes]
isb ps [SERVICE...] [--json]
isb exec SERVICE -- ARGV...
isb config
```

`isb exec` exits with the command's status. If isb itself fails (the sandbox
does not exist, incusd is unreachable) it exits 125.

`prune` is a dry run unless given `-y`, and never touches a sandbox without the
label or whose path still exists.

A sandbox is a container unless it says `type: vm` (or `isb create --vm`).
VMs need a VM image (`images:debian/13/cloud`, `images:ubuntu/24.04/cloud`, or a
local one), boot in tens of seconds rather than one or two, and wait for the
incus agent before `exec` works. See [examples/vm.yaml](examples/vm.yaml) and
"Containers vs virtual machines" in the spec for what differs.

The YAML format is documented field by field in [docs/spec.md](docs/spec.md).
[examples/lasso-dev.yaml](examples/lasso-dev.yaml) is a complete real-world
example (lasso's per-worktree frontend dev containers), with
[examples/lasso-dev-web.yaml](examples/lasso-dev-web.yaml) as an overlay for
the dev server's port forwards.

## Library

```rust,no_run
use isb::{Client, ExecOptions, PortBinding, ReadyCheck, Sandbox, SandboxSpec, Volume};

fn main() -> isb::Result<()> {
    let client = Client::new();
    let spec = SandboxSpec::new("dev-web", "dev-base")
        .cpus(8)
        .memory("8GiB")
        .label("app", "web")
        .volume("/home/dev/src", Volume::bind("/srv/src").device("src"))
        .volume("/home/dev/.cache", Volume::named("dev-cache").owner("dev"))
        .port(PortBinding::host("tcp:127.0.0.1:5173", "tcp:127.0.0.1:5173"))
        .port(PortBinding::guest("tcp:127.0.0.1:8190", "tcp:127.0.0.1:8080"))
        .ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]);

    let sb = Sandbox::connect_or_create(&client, &spec)?; // reconciles
    let out = sb.exec_with(["id", "-un"], ExecOptions::default().user("dev"))?;
    assert_eq!(out.stdout_text().trim(), "dev");

    for ev in sb.exec_stream(["make", "test"], ExecOptions::default())? {
        // ExecEvent::Stdout / ExecEvent::Stderr, as produced
        let _ = ev;
    }
    Ok(())
}
```

The main entry points: `Sandbox::create`, `Sandbox::connect_or_create` (creates
or reconciles), `get`, `list_with`, `remove`, `start`, `stop`, `exec`,
`exec_stream`, `attach`, `add_port`; `Volume::bind` / `Volume::named` and
`PortBinding::host` / `PortBinding::guest` to build specs; plan/apply via
`isb::sandbox::{resolve, plan_desired, ensure}`; compose files via
`isb::compose::load`.

## Development

```sh
cargo test                          # unit tests; integration tests skip themselves
ISB_INTEGRATION=1 cargo test        # against the real incusd
```

The integration tests need a local image with a `dev` user at uid 1000 and
`python3` (`ISB_TEST_IMAGE`, default `dev-base`). Everything they create is named
`isb-test-*` and is removed afterwards, pass or fail; they touch nothing else.
The incus socket is root-equivalent, so on a shared host build the test
binaries in a sandbox without it (`cargo test --no-run`) and run them on the
host.

## License

[MIT](LICENSE)
