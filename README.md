# isb

[![crates.io](https://img.shields.io/crates/v/isb.svg)](https://crates.io/crates/isb)
[![PyPI](https://img.shields.io/pypi/v/isb-sdk.svg)](https://pypi.org/project/isb-sdk/)
[![npm](https://img.shields.io/npm/v/@execution-associates/isb.svg)](https://www.npmjs.com/package/@execution-associates/isb)
[![CI](https://github.com/execution-associates/isb/actions/workflows/ci.yml/badge.svg)](https://github.com/execution-associates/isb/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Sandboxes you can describe, on your own Linux machine.** Write down the
containers or VMs you want in a small YAML file, or in Python, TypeScript or
Rust, and isb makes [incus](https://linuxcontainers.org/incus/) match it:
creating what is missing and changing only what differs, every time you run it.

```yaml
# isb.yaml
services:
  web:
    image: images:ubuntu/24.04
    cpus: 2
    mem_limit: 2g
    idmap: auto                  # files you create inside stay yours outside
    volumes:
      - ./site:/home/ubuntu/site
    ports:
      - "8000:8000"              # on the host's 127.0.0.1, unless you name an address
    ready: [running, default_route]
    user: ubuntu
    working_dir: /home/ubuntu/site
    command: python3 -u -m http.server 8000
```

If you know docker compose you know the format: the same keys and syntax, plus
a few incus-only ones (`type: vm`, `idmap`, `ready`). The [reference](docs/spec.md)
lists the handful of places it differs, such as ports defaulting to localhost.

```console
$ isb up                  # create it (or fix only what drifted), then run `command`
web | Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/) ...
^C                        # stops the sandbox; the next `isb up` starts it again
$ isb down                # delete it, host untouched
```

Like `docker compose up`, `isb up` stays in the foreground: it streams each
service's `command` with a `<service> | ` prefix and stops the sandboxes when
the commands exit, on Ctrl-C, or when whatever started isb goes away. Scripts
that want the sandbox up and then carry on use `-d`:

```console
$ isb up -d                                 # create or reconcile, wait until ready, return
$ isb exec web -- python3 -m unittest       # as the service's user, in its working_dir
$ isb down
```

## Long-running services

Add `restart: always` and a service's `command` is supervised inside its
guest instead of by `isb up`: it is restarted when it crashes and comes back
with the host, with nothing of isb's running. For more than that, deploy the
same file as a **stack** to the `isb serve` daemon, a docker swarm for one
host:

```yaml
services:
  api:
    image: docker:node:22        # an OCI image, or any incus image (dev-base, images:debian/12)
    command: [node, server.js]
    ports: ["127.0.0.1:8080:8080"]
    healthcheck: {test: [CMD, curl, -fsS, http://127.0.0.1:8080/health]}
    deploy:
      replicas: 3
      update_config: {order: start-first}
```

```console
$ isb serve install          # the daemon, as a systemd user service
$ isb stack deploy app       # replicas, load-balanced, health-checked
$ isb stack ps app
$ isb stack rollback app
```

The daemon load-balances published ports over healthy replicas, restarts
unhealthy ones, rolls out changes with no downtime and rolls them back, and
resumes every stack when it restarts. `isb tui` shows all of it live, rollouts
slot by slot, and drives it: logs, shells, scaling, deploys and rollbacks
([docs/tui.md](docs/tui.md)). It also serves the same operations as
**MCP tools**, so an agent behind Cloudflare Access can deploy apps and run
sandboxes, held to a policy that keeps the host out of its reach. See
[docs/stacks.md](docs/stacks.md) and [docs/serve.md](docs/serve.md).

## Why incus

incus runs **system containers**: a whole Linux machine, with its own init,
users, services and network, rather than a single process. That makes it a
natural fit for sandboxes that people and agents actually work in.

- **Feels like a VM, starts like a container.** A container is usable in a few
  seconds (about 5 on a busy host, image cached) and costs almost nothing when
  idle. Leave it running.
- **VMs with the same tool.** Change `type: container` to `type: vm` when you
  want a separate kernel between the code and your machine. Same file, same
  commands, same API.
- **Unprivileged by default.** Root inside a container is an ordinary user
  outside it, in its own user namespace. Mount only the directories a sandbox
  needs.
- **Real networking.** Every sandbox gets its own address on a private bridge,
  plus port forwards in either direction: publish a guest port on the host, or
  let the guest reach one host service and nothing else.
- **Yours.** It runs on any Linux box you control, laptop to server, with no
  account, no cloud and no per-minute bill. incus is open source (Apache 2.0)
  and maintained by the Linux Containers project.

## What isb adds

incus has the machinery. isb makes it declarative and dependable:

- **Converges, never churns.** `isb up` compares what you asked for with what
  exists and changes only the difference. A mount that is already right is never
  touched, so a dev server's file watching (hot reload) keeps working through
  every `up`. `isb plan` shows the difference first.
- **Nothing hangs silently.** Every call to incus has a deadline, and a stall is
  reported as the step that stalled ("create instance web stalled after 600s"),
  not a terminal that sits there.
- **`exec` that behaves.** Arguments stay a list, never glued into a shell
  string. Output streams as it is produced. Exit codes, stdin, a real terminal
  when you have one, and Ctrl-C all work, and a command that does not read stdin
  never waits for it.
- **Nothing left running by accident.** A foreground `isb up` notices when the
  process that started it exits, even when no signal arrives (an agent's
  background task, a closed terminal), and stops its sandboxes. A dev server
  never outlives the session that wanted it.
- **Ready means ready.** Wait for the network, a user, a writable path or your
  own check before the first command, not just for "running".
- **Made for many sandboxes at once.** Labels to find them, `prune` to delete
  the ones whose project directory is gone, and a lock so two tasks never create
  the same one twice.
- **One engine, four ways in.** The CLI, Rust, Python and TypeScript all drive
  the same core, so they behave identically. It ships as a single static binary.
- **A web UI in the same binary.** `isb serve` serves a browser UI next to its
  API: sign in with a password, a passkey, GitHub, Google or your SSO, invite
  people to an org, make API tokens, and watch stacks update live
  ([docs/web.md](docs/web.md)).

## Good for

- **AI agents and untrusted code.** Give each agent or task its own machine with
  one project directory mounted, instead of your whole home directory and its
  credentials. An agent that runs `isb up` as a background task gets its dev
  server stopped when the agent goes away.
- **A dev environment per branch.** One sandbox per git worktree, each with its
  own dependencies and dev server, side by side. lasso runs its frontend
  tooling this way ([examples/lasso-dev.yaml](examples/lasso-dev.yaml)).
- **Apps that run for months.** Replicated, health-checked, load-balanced
  services with rolling updates, from the same file you develop with.
- **Throwaway test machines.** Real init and services, created from code in
  seconds, removed just as fast.

## Install

isb needs a Linux host with [incus](https://linuxcontainers.org/incus/docs/main/installing/)
installed, and access to its socket (usually membership in the `incus-admin`
group, which is root-equivalent on that host).

```sh
# The CLI: a static binary, nothing to compile
mise use -g github:execution-associates/isb
# or build it: cargo install isb

# The SDKs (each bundles the binary)
pip install isb-sdk                  # imported as `isb`
bun add @execution-associates/isb    # or npm install

# The Rust library
cargo add isb
```

Prebuilt x86_64 and aarch64 binaries are also on the
[releases page](https://github.com/execution-associates/isb/releases).

**On macOS**, isb runs incus in a Lima VM it manages: `brew install lima`,
then `isb machine init`. Your home directory is shared at the same path and
published ports reach the Mac's localhost, so everything below works as on
Linux. See [docs/macos.md](docs/macos.md).

## Use it from code

### Python

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

    out = await sb.exec("uname", ["-a"])
    print(out.stdout_text)

    proc = await sb.exec_stream(["sh", "-c", "for i in 1 2 3; do echo $i; sleep 1; done"])
    async with proc:
        async for event in proc:
            print(event.text, end="")
        print("exit code:", await proc.wait())

    await sb.remove(force=True)


asyncio.run(main())
```

More in [sdk/python](sdk/python): stdin, compose files, errors.

### TypeScript

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

More in [sdk/typescript](sdk/typescript).

### Rust

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

API docs on [docs.rs/isb](https://docs.rs/isb).

## Variables

`${VAR}` in a YAML file is filled in from the environment when it is loaded,
the way docker compose does it:

```console
$ WORKTREE=$PWD isb up
$ isb --env-file dev.env up       # KEY=VALUE lines; the environment wins
$ isb up                          # reads .env next to isb.yaml, if there is one
```

`${VAR:-default}` supplies a default, `${VAR:?message}` fails with a message,
and a plain `${VAR}` that is unset is an error rather than an empty string. The
SDKs take a `vars` map as well. `isb config` prints the file with every variable
filled in.

## Documentation

- [docs/spec.md](docs/spec.md): every YAML field, and how reconciling works
- [docs/cli.md](docs/cli.md): every command and flag
- [docs/stacks.md](docs/stacks.md): long-running stacks: replicas, health, rollouts
- [docs/serve.md](docs/serve.md): the `isb serve` daemon and its MCP server behind Cloudflare Access
- [docs/secrets.md](docs/secrets.md): per-org secrets: the age-encrypted store, break-glass recipients, `isb secret`
- [docs/auth.md](docs/auth.md): users, roles, sessions, invitations and API tokens for `isb serve`
- [docs/web.md](docs/web.md): the web UI: how it is built, embedded and served, and how to develop it
- [docs/tui.md](docs/tui.md): `isb tui`, the live dashboard
- [docs/macos.md](docs/macos.md): isb on a Mac, with `isb machine`
- [docs/rpc.md](docs/rpc.md): the protocol the SDKs speak, for other languages
- [examples/](examples): a real per-worktree dev setup, and a VM
- [SKILL.md](SKILL.md): an agent skill for isb. Put it in your agent's skills
  directory (for Claude Code, `~/.claude/skills/isb/SKILL.md`) and agents will
  use isb correctly

## Development

```sh
cargo test                          # unit tests; integration tests skip themselves
ISB_INTEGRATION=1 cargo test        # against a real incusd
```

The integration tests need a local image with a `dev` user at uid 1000 and
`python3` (`ISB_TEST_IMAGE`, default `dev-base`). Everything they create is
named `isb-test-*` and is removed afterwards, pass or fail. The incus socket is
root-equivalent, so on a shared host build the test binaries in a sandbox
without it (`cargo test --no-run`) and run them on the host.

## License

[MIT](LICENSE)
