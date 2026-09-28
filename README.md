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
```

```console
$ isb plan                  # what would change
$ isb up                    # create, or change only what differs
$ isb exec web -- bun install
$ isb down
```

## Why

isb is a fresh implementation inspired by
[incus-sandbox-sdk](https://github.com/zoid-archive/incus-sandbox-sdk), which
shells out to the `incus` CLI. That approach was the wrong shape for long-lived
dev containers:

- **Every request has a deadline.** A CLI client that hangs (one `incus init`
  sat about 11 minutes with no matching server operation) cannot be bounded from
  outside. isb sets a socket timeout on every request and waits on every
  mutation as a server operation, so a stall is reported as the step that
  stalled (`create instance web stalled: operation ... still running after
  600s`). A create that stalls is cleaned up only if this call created it (a
  token in `user.isb.create-token`), then retried once.
- **Config before first boot.** The instance is created in one request with its
  config and devices, so every mount, volume, label and idmap exists before the
  first boot.
- **A correct device is never touched.** Re-adding a disk device remounts it,
  which silently kills inotify watches a running dev server holds (Vite keeps
  answering 200 while HMR goes quiet). `isb up` compares each device and patches
  only mismatches, under deterministic names. The integration tests hold a live
  inotify watch across a no-op `up` to prove it.
- **exec that behaves.** argv is passed as a list, never joined into `sh -c`.
  Output streams as produced, with no default timeout. stdin is forwarded and
  closed properly: a command that does not read stdin returns at once even when
  isb's own stdin is a pipe that never reaches EOF. TTY when stdin and stdout are
  terminals, signals forwarded, exit code propagated.
- **No JavaScript on the host.**

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
isb create NAME -i IMAGE [--cpus N] [-m MEM] [-v SRC:GUEST[:ro,owner=U]] [-p [IP:]HOST:GUEST]
                         [-l k=v] [-e K=V] [--idmap auto] [--ready CHECK] [--ensure]
isb start|stop|restart|rm NAME...
isb ls [--label k[=v]] [--json]            list, filtered by label
isb inspect NAME [--json]
isb exec NAME|SERVICE [-u USER] [-w DIR] [-e K=V] [-l] [-t|-T] [-n] [--timeout D] -- ARGV...
isb volume create|ls|inspect|rm
isb port add NAME SPEC [--name DEV] [--search N]   prints the listen address in use
isb port rm NAME DEV... | isb port ls NAME
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

Names follow microsandbox where the semantics match: `Sandbox::create`,
`connect_or_create` (which reconciles), `get`, `list_with`, `remove`, `exec`,
`exec_stream`, `attach`, `start`, `stop`, `Volume::bind`, `Volume::named` with
`NamedVolumeMode`, `PortBinding`. isb extensions: readiness checks, reverse
(`bind: guest`) port bindings, idmap, storage pools, `raw_config` /
`raw_devices`, plan/apply (`isb::sandbox::{resolve, plan_desired, ensure}`), and
compose loading (`isb::compose::load`).

## Not supported, by design

Some microsandbox features have no safe incus equivalent, and isb adds no
field that approximates them:

- Destination-bound secrets (a secret substituted only on connections to one
  host). Never put a secret in `env`: that is plain instance config, readable by
  anyone who can read the instance.
- Domain-based egress rules.
- Full-memory snapshots.

## Dependencies

Kept few and well known. Each one earns its place:

| Crate | Why |
|---|---|
| `serde`, `serde_json` | The incus REST API is JSON; the spec model is serde. |
| `serde_yaml_ng` | The compose format. A maintained fork of `serde_yaml` (which is archived); chosen over `serde_yml`, which has soundness advisories. |
| `schemars` | Generates the JSON Schema (`isb schema`) from the same types, so docs, validation and code cannot drift. |
| `thiserror` | Error type boilerplate. |
| `clap` | The CLI. |
| `tungstenite` (no TLS features) | incus exec streams stdio over websockets. Used synchronously over the unix socket. |
| `httparse` | Parses HTTP responses for the small built-in HTTP/1.1 client (already a `tungstenite` dependency). Avoids pulling in an async runtime and a full HTTP stack. |
| `rustix` | flock, termios (raw mode, window size), poll, isatty. Safe wrappers instead of `unsafe` libc calls. |
| `signal-hook` | Forwarding SIGINT/SIGTERM/SIGHUP/SIGWINCH to the command. |
| `tempfile` (dev only) | Tests. |

CI runs `cargo deny` (advisories, licenses, bans, sources) and `cargo audit`.

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

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
