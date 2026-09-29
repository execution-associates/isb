# isb

Declarative incus sandboxes, containers or VMs: describe them in a
compose-style YAML file or in code, and isb creates them, then changes only what
differs on every run. It is a Rust library and a CLI, with Python and TypeScript
SDKs. It needs a running incus daemon, but not the `incus` command-line client.

```yaml
# isb.yaml
sandboxes:
  web:
    image: dev-base
    cpus: 8
    memory: 8GiB
    idmap: auto
    labels:
      app.worktree: "${WORKTREE}"
    volumes:
      /home/dev/src: { bind: ./src, device: src }
      /home/dev/.bun/install/cache: { named: bun-cache, owner: dev }
    ports:
      - name: vite
        bind: host
        listen: "${IP}:5173"
        connect: 5173
        search: 50
    ready: [running, default_route, { user_exists: dev }]
    exec:
      user: dev
      cwd: /home/dev/src

  # The same, as a virtual machine: its own kernel instead of the host's.
  worker:
    type: vm
    image: images:debian/13/cloud
    cpus: 4
    memory: 4GiB
    volumes:
      /srv/data: { bind: ./data }
    ready: [running, agent, default_route]

volumes:
  bun-cache: {}
```

```console
$ isb plan                  # what would change
$ isb up                    # create, or change only what differs
$ isb exec web -- bun install
$ isb down
```

## Install

Prebuilt static binaries for x86_64 and aarch64 Linux are attached to each
[GitHub release](https://github.com/execution-associates/isb/releases).

```sh
# Prebuilt binary, no compiling
mise use -g github:execution-associates/isb

# From crates.io (builds from source)
cargo install isb
mise use -g cargo:isb

# The library
cargo add isb
```

Building from source runs build scripts and proc macros from dependencies.
Build inside a sandbox if that matters to you (see [Development](#development)).

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
        .port(PortBinding::host("5173", "5173"))
        .port(PortBinding::guest("8190", "8080"))
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

## Python and TypeScript

The SDKs live in this repository and drive the same engine through `isb rpc`, a
line-delimited JSON protocol on the binary's stdin/stdout
([docs/rpc.md](docs/rpc.md)). Any other language can use that protocol too.

- Python: [sdk/python](sdk/python), `pip install isb-sdk` (imported as `isb`)
- TypeScript (Bun): [sdk/typescript](sdk/typescript), `bun add @execution-associates/isb`

Both are async, have no runtime dependencies, and ship the static isb binary
for x86_64 and aarch64 Linux, so nothing else needs installing.

### Python

```python
import asyncio

from isb import PortBinding, Project, Sandbox, Volume


async def main() -> None:
    # Create the sandbox, or change only what differs if it already exists.
    sb = await Sandbox.connect_or_create(
        "web",
        image="dev-base",
        cpus=2,
        memory="2GiB",
        idmap="auto",
        labels={"app": "web"},
        volumes={
            "/home/dev/src": Volume.bind("./src", device="src"),
            "/home/dev/.cache": Volume.named("web-cache", owner="dev"),
        },
        ports=[
            PortBinding.host("5173", "5173", name="vite", search=20),
        ],
        ready=["running", "default_route", {"user_exists": "dev"}],
        exec={"user": "dev", "cwd": "/home/dev/src"},
    )

    # Run a command and collect its output. argv is passed as a list and is
    # never joined into a shell string.
    out = await sb.exec("uname", ["-a"])
    print(out.exit_code, out.stdout_text)

    # Stream output as it is produced, feeding stdin as you go.
    proc = await sb.exec_stream(
        ["sh", "-c", "cat; echo done >&2"],
        stdin="piped",
    )
    async with proc:
        await proc.write(b"hello\n")
        await proc.close_stdin()
        async for event in proc:
            print(event.kind, event.text, end="")
        print("exit code:", await proc.wait())

    # Find sandboxes by label, then clean up.
    for info in await Sandbox.list_with(labels={"app": "web"}):
        print(info.name, info.status)
    await sb.remove(force=True)

    # Or drive a compose file, like `isb up` / `isb down`.
    project = await Project.load(
        "isb.yaml",
        vars={"WORKTREE": "/srv/wt", "IP": "127.0.0.1"},
    )
    for service, report in await project.up(on_progress=print):
        print(service, report.created, report.ports)
    await project.sandbox("web").exec(["ls", "-la"])
    await project.down(volumes=True)


asyncio.run(main())
```

### TypeScript

```ts
import {
  PortBinding,
  Project,
  Sandbox,
  Volume,
} from "@execution-associates/isb";

// Create the sandbox, or change only what differs if it already exists.
const sb = await Sandbox.connectOrCreate({
  name: "web",
  image: "dev-base",
  cpus: 2,
  memory: "2GiB",
  idmap: "auto",
  labels: { app: "web" },
  volumes: {
    "/home/dev/src": Volume.bind("./src", { device: "src" }),
    "/home/dev/.cache": Volume.named("web-cache", { owner: "dev" }),
  },
  ports: [
    PortBinding.host("5173", "5173", { name: "vite", search: 20 }),
  ],
  ready: ["running", "default_route", { user_exists: "dev" }],
  exec: { user: "dev", cwd: "/home/dev/src" },
});

// Run a command and collect its output. argv is passed as a list and is
// never joined into a shell string.
const out = await sb.exec("uname", ["-a"]);
console.log(out.exitCode, out.stdoutText);

// Stream output as it is produced, feeding stdin as you go.
const proc = await sb.execStream(["sh", "-c", "cat; echo done >&2"], {
  stdin: "piped",
});
await proc.write("hello\n");
await proc.closeStdin();

const decoder = new TextDecoder();
for await (const event of proc) {
  process.stdout.write(`${event.kind}: ${decoder.decode(event.data)}`);
}
console.log("exit code:", await proc.wait());

// Find sandboxes by label, then clean up.
for (const info of await Sandbox.listWith({ labels: { app: "web" } })) {
  console.log(info.name, info.status);
}
await sb.remove({ force: true });

// Or drive a compose file, like `isb up` / `isb down`.
const project = await Project.load({
  files: ["isb.yaml"],
  vars: { WORKTREE: "/srv/wt", IP: "127.0.0.1" },
});
for (const { service, report } of await project.up()) {
  console.log(service, report.created, report.ports);
}
await project.sandbox("web").exec(["ls", "-la"]);
await project.down({ volumes: true });
```

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
