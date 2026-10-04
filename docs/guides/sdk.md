---
title: isb from code
nav_title: SDKs
description: Create sandboxes, run commands and load isb.yaml files from Python, TypeScript or Rust.
order: 18
---

Everything `isb up` and `isb exec` do is available from code, for test
harnesses, CI tools and agent frameworks that make sandboxes on the fly. The
Python and TypeScript SDKs drive the isb binary over `isb rpc`, a
line-delimited JSON protocol on its stdin and stdout ([The rpc
protocol](../reference/rpc.md)); the Rust crate is isb itself. All three use
the same engine, so a spec behaves the same whether it came from YAML, Python
or TypeScript, and the spec objects use the compose file's field names
exactly ([isb.yaml reference](../reference/compose.md)).

Like the CLI, they need access to the incus socket (`$INCUS_SOCKET`, else
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`), which usually
means membership in `incus-admin`, and that access is root-equivalent on the
host.

## Python

```sh
pip install isb-sdk          # imported as `isb`
```

Python 3.10 or later on Linux, no runtime dependencies, typed. The platform
wheels (x86_64 and aarch64 Linux) bundle a static isb binary; otherwise the
binary is found as `Client(isb_bin=...)`, then `$ISB_BIN`, then the bundled
one, then `isb` on `PATH`.

```python
import asyncio

from isb import Sandbox, Volume


async def main() -> None:
    sb = await Sandbox.connect_or_create(
        "web",
        image="images:ubuntu/24.04",
        cpus=2,
        idmap="auto",
        volumes=[Volume.bind("./site", "/home/ubuntu/site")],
        ports=["8000:8000"],
        user="ubuntu",
        working_dir="/home/ubuntu/site",
    )

    out = await sb.exec("uname", ["-a"])          # argv, never a shell string
    print(out.stdout_text)

    proc = await sb.exec_stream(["sh", "-c", "for i in 1 2 3; do echo $i; sleep 1; done"])
    async with proc:
        async for event in proc:
            print(event.text, end="")
        print("exit code:", await proc.wait())

    await sb.remove(force=True)


asyncio.run(main())
```

A compose file:

```python
import isb

project = await isb.Project.load("isb.yaml", vars={"WORKTREE": "/srv/wt"})
for plan in await project.plan():
    print(plan.name, plan.status, plan.actions)
for service, report in await project.up(on_progress=print):   # like `isb up -d`
    print(service, report.created, report.ports)
await project.sandbox("web").exec(["bun", "install"])          # with the service's exec defaults
await project.down(volumes=True)
```

The full API (the client, `Sandbox`, `exec` and `exec_stream`, the `Volume`
and `PortBinding` builders, `Project`, volumes, prune and the error classes) is
in [sdk/python/README.md](https://github.com/execution-associates/isb/blob/main/sdk/python/README.md).

## TypeScript

```sh
bun add @execution-associates/isb      # or npm install
```

Bun first; Node 20 or later works too. ESM only, no runtime dependencies. The
optional packages `@execution-associates/isb-linux-x64` and `-linux-arm64`
carry the binary; otherwise it is found as `new Client({ isbBin })`, then
`$ISB_BIN`, then the platform package, then `isb` on `PATH`.

```ts
import { Sandbox, Volume } from "@execution-associates/isb";

const sb = await Sandbox.connectOrCreate({
  container_name: "web",
  image: "images:ubuntu/24.04",
  cpus: 2,
  idmap: "auto",
  volumes: [Volume.bind("./site", "/home/ubuntu/site")],
  ports: ["8000:8000"],
  user: "ubuntu",
  working_dir: "/home/ubuntu/site",
});

const out = await sb.exec("uname", ["-a"]);
console.log(out.stdoutText);

const proc = await sb.execStream(["sh", "-c", "for i in 1 2 3; do echo $i; sleep 1; done"]);
const decoder = new TextDecoder();
for await (const event of proc) {
  process.stdout.write(decoder.decode(event.data));
}
console.log("exit code:", await proc.wait());

await sb.remove({ force: true });
```

`Project.load({ files, vars })` with `plan()`, `up()`, `down()` and
`sandbox(service)` works as in Python. The full API is in
[sdk/typescript/README.md](https://github.com/execution-associates/isb/blob/main/sdk/typescript/README.md).

## Rust

```sh
cargo add isb
```

```rust
use isb::{Client, Sandbox, SandboxSpec, Volume};

fn main() -> isb::Result<()> {
    let client = Client::new();
    let spec = SandboxSpec::new("web", "images:ubuntu/24.04")
        .cpus(2)
        .volume("/home/ubuntu/site", Volume::bind("./site"));

    let sb = Sandbox::connect_or_create(&client, &spec)?;
    let out = sb.exec(["uname", "-a"])?;
    print!("{}", out.stdout_text());
    Ok(())
}
```

API docs are on [docs.rs/isb](https://docs.rs/isb).

## What every SDK shares

- **Converge, don't recreate.** `connect_or_create` / `connectOrCreate`
  creates what is missing and changes only what differs; a correct mount is
  never remounted. `plan` shows the difference first.
- **argv stays a list.** Nothing is joined into a shell string; use
  `["sh", "-c", "..."]` when you want a shell. A non-zero exit is a result,
  not an exception.
- **Exec defaults travel with the spec.** A handle from `connect_or_create`
  or `Project.sandbox` runs commands as the spec's `user`, in its
  `working_dir`, with its `exec.env`; per-call arguments override them.
- **Streaming** gives output as it is produced, stdin writes, signals and
  terminal resizes.
- **A service's `command`** is for the foreground CLI `isb up`; `Project.up`
  behaves like `isb up -d` and does not run it.
- **Errors carry a stable code** (`not_found`, `already_exists`,
  `not_ready`, `connect`, ...) from [the rpc protocol](../reference/rpc.md#errors).

Another language can speak the same protocol: start `isb rpc`, read its hello
line, and send one JSON request per line.

## Driving `isb serve` instead

The SDKs manage sandboxes on the local incus. To deploy apps or stacks to an
`isb serve` daemon, possibly remote, call its tools over REST or MCP with an
API token ([HTTP API](../reference/http-api.md), [Agents and MCP](agents.md)):

```sh
curl -sS https://isb.example.com/orgs/acme/api/v1/tools/app_deploy \
  -H "Authorization: Bearer $ISB_TOKEN" -H "Content-Type: application/json" \
  -d '{"name": "web", "wait": true}'
```
