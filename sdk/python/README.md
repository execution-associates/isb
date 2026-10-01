# isb for Python

An asyncio SDK for [isb](https://github.com/execution-associates/isb):
declarative incus sandboxes (containers and VMs). It is a thin client of
`isb rpc`, a line-delimited JSON protocol over the isb binary's stdin and stdout
([docs/rpc.md](https://github.com/execution-associates/isb/blob/main/docs/rpc.md)).
All the work (planning, reconciling, readiness, exec) happens in isb; this
package starts it, sends requests and maps the answers to Python types.

- Python 3.10 or later, Linux.
- No runtime dependencies (standard library only).
- Typed (`py.typed`), with TypedDicts for the spec generated from `isb schema`.

## Install

```sh
pip install isb-sdk
```

Platform wheels (x86_64 and aarch64 Linux) bundle a static isb binary at
`isb/_bin/isb`, so nothing else is needed. The pure wheel and the sdist do not;
they use an isb binary from elsewhere.

The binary is looked up in this order:

1. `Client(isb_bin="/path/to/isb")`
2. the `ISB_BIN` environment variable
3. the bundled binary
4. `isb` on `PATH`

It must be a version with the `rpc` command (protocol 1). The client refuses a
server that announces any other protocol version.

isb needs access to the incus socket (`$INCUS_SOCKET`, else
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`), which usually
means membership in `incus-admin`. That access is root-equivalent on the host.

## Quickstart

```python
import asyncio
import isb
from isb import PortBinding, Sandbox, Volume


async def main() -> None:
    async with isb.Client() as client:
        sb = await Sandbox.connect_or_create(
            "dev-web",
            image="dev-base",
            client=client,
            cpus=4,
            mem_limit="4g",
            idmap="auto",
            labels={"app": "web"},
            environment={"NODE_ENV": "development"},
            volumes=[
                Volume.bind("./src", "/home/dev/src", device="src"),
                Volume.named("dev-cache", "/home/dev/.cache", owner="dev"),
            ],
            ports=[
                PortBinding.publish("5173-5193", 5173, name="vite"),  # first free port
                "8080:80",  # docker's short syntax works too
            ],
            ready=["running", "default_route", {"user_exists": "dev"}],
            user="dev",
            working_dir="/home/dev/src",
            on_progress=print,
        )
        print(sb.last_report)

        # Captured output. argv is never joined into a shell string.
        out = await sb.exec("printf", ["[%s]", "a b", "$HOME"])
        assert out.stdout_text == "[a b][$HOME]"

        # Streaming output, as it is produced.
        script = "for i in 1 2 3; do echo $i; sleep 1; done"
        proc = await sb.exec_stream(["sh", "-c", script])
        async with proc:
            async for event in proc:
                print(event.kind, event.text, end="")
            print("exit code:", await proc.wait())

        await sb.remove(force=True)


asyncio.run(main())
```

### Compose files

```python
project = await isb.Project.load("isb.yaml", vars={"WORKTREE": "/srv/wt"})

for plan in await project.plan():
    print(plan.name, plan.status, plan.actions)

# Like `isb up -d`: returns once the sandboxes are ready. A service's
# `command` is for the foreground CLI `isb up` and is not run here.
for service, report in await project.up(on_progress=print):
    print(service, report.created, report.ports)

# A sandbox from the project carries its service's exec defaults.
web = project.sandbox("web")
await web.exec(["bun", "install"])

await project.down(volumes=True)
```

## API overview

### Client

`Client(isb_bin=None, socket=None, project=None, create_timeout=None)` owns one
`isb rpc` subprocess, started on first use (or `await client.start()`) and
stopped by `await client.close()` or `async with`. `socket`, `project` and
`create_timeout` are passed as the global flags `--socket`, `--project` and
`--create-timeout`. Requests run concurrently over the one process.

Every function takes an optional `client=`. Without one it uses
`isb.default_client()`, created lazily with default settings (one per event
loop). `client.call(method, params)` sends any protocol method directly.

A client belongs to the event loop it started on. If the subprocess exits,
every pending and later request fails with `ProcessError`, whose message
includes the tail of isb's stderr.

The server's working directory is fixed when it starts, so the SDK resolves
relative paths itself: `base_dir` for bind mounts defaults to the current
directory at the time of the call, and compose file paths are made absolute.

### Sandbox

| | |
|---|---|
| `await Sandbox.create(name, *, image, **spec)` | Create. `AlreadyExistsError` if the name is taken. |
| `await Sandbox.connect_or_create(name, *, image, prune_devices=False, **spec)` | Create or reconcile (only what differs changes). The report is on `sb.last_report`. Also `Sandbox.ensure`. |
| `await Sandbox.plan(name, *, image, **spec)` | What `connect_or_create` would do, as a `Plan`. |
| `await Sandbox.get(name)` | Handle on an existing sandbox. `NotFoundError` if missing. |
| `await Sandbox.list_with(labels)` / `Sandbox.list()` | `list[SandboxInfo]`. `labels` is `{"k": "v", "k2": None}` or `["k=v", "k2"]`. |
| `await Sandbox.remove(name, force=False)` | Delete. A running sandbox needs `force`. |

`name` is the incus instance name (the spec's `container_name`). `**spec` are
the fields of a compose service
([docs/spec.md](https://github.com/execution-associates/isb/blob/main/docs/spec.md)),
named as in docker compose: `cpus` (a count) or `cpuset` (`"0-3"`),
`mem_limit` (`"512m"`, `"8g"`, `"8GiB"`), `storage`, `type` (`"container"`,
`"virtual-machine"` or `"vm"`), `privileged`, `idmap`, `incus_profiles`,
`labels` and `environment` (a map or a `["KEY=VALUE"]` list), `volumes` and
`ports` (lists; see Builders), `user`, `working_dir`, `exec` (`env` for exec
only, `login`), `command` (a string or argv), `ready`, `ready_timeout`,
`raw_config`, `raw_devices`. A full spec dict can be passed as `spec=` instead.
The server rejects unknown fields (`InvalidError`). The create-style methods
also take `base_dir`, `wait_ready` (default true), `named_volumes` and
`on_progress` (called with each progress line). `named_volumes` are top-level
volume definitions, as in a compose file's `volumes:`: a mount's source names
a key, and the incus volume is that definition's `name`, else the key itself.

On an instance: `info()`, `labels()`, `start()`, `stop(force=False, timeout="30s")`,
`restart()`, `wait_ready(ready=None, ready_timeout=None)`, `remove(force=False)`,
`add_port(port)` (a `ports` entry; returns the listen address in use),
`remove_device(name)` (returns whether it existed).

A handle from `create`, `connect_or_create` or `Project.sandbox` carries the
exec defaults its spec implies (`user`, `working_dir`, `exec.env`,
`exec.login`) as `sb.exec_defaults` (an `ExecDefaults` dict of `user`, `cwd`,
`env`, `login`) and sends them with every exec; per-call arguments override
them.

### Exec

```python
out = await sb.exec(
    cmd,
    args=None,
    *,
    cwd=None,
    user=None,
    env=None,
    login=None,
    timeout=None,
    stdin=None,
    tty=False,
)
```

`cmd` is a program plus `args`, or a full argv list. Returns
`ExecOutput(exit_code, stdout: bytes, stderr: bytes)` with `stdout_text`,
`stderr_text` and `success`. A non-zero exit is not an exception. `stdin` is
bytes or str, sent followed by EOF. `timeout` is seconds or a duration string
(`"90s"`); when it passes, the command is killed and `IsbTimeoutError` is
raised. With `tty=True`, all output arrives as stdout.

```python
p = await sb.exec_stream(cmd, args=None, *, ..., stdin=None | "piped" | bytes)
```

returns an `ExecProcess`: iterate it for `ExecEvent(kind, data)` chunks in
order, and drive it with `await p.write(b)`, `await p.close_stdin()`,
`await p.signal(15)`, `await p.resize(w, h)`. `await p.wait()` returns the exit
code, `await p.collect()` gathers the rest into an `ExecOutput`. Used as
`async with`, it kills the command on exit if it is still running.

### Builders

These return plain dicts, one `volumes` or `ports` list entry each. Docker's
short strings work in the same lists: `"./src:/home/dev/src:ro"` (options `ro`,
`rw`, `owner=USER`, `device=NAME`, `pool=POOL`, `external`; a source starting
with `/`, `.` or `~` is a host path, anything else a named volume) and
`"[HOST_IP:]PUBLISHED:TARGET[/udp]"` (HOST_IP defaults to 127.0.0.1).

- `Volume.bind(source, target, *, read_only=False, device=None, options=None)`:
  bind-mount a host path.
- `Volume.named(source, target, *, external=False, owner=None, read_only=False, pool=None, device=None, options=None)`:
  mount a named volume, created if missing unless `external`; `owner` chowns
  the mount point.
- `PortBinding.publish(published, target, *, host_ip=None, protocol=None, name=None, options=None)`:
  publish a guest port on the host. A `published` range (`"5173-5223"`) with
  one `target` takes the first free port; `ApplyReport.ports` says which.
- `PortBinding.host(listen, connect, *, name=None, search=None, options=None)`:
  an incus proxy listening on the host, connecting in the guest, for addresses
  `publish` cannot express (`unix:` sockets, a connect host). `search=N` turns
  a single listen port P into the published range `P-(P+N)`, so it needs a
  single TCP/UDP listen port and a connect port with no host of its own
  (ValueError otherwise).
- `PortBinding.guest(listen, connect, *, name=None, options=None)`: listen in
  the guest, connect on the host.

### Project

`await Project.load(files=None, *, env_files=None, project_name=None, vars=None)`
loads and resolves compose files (default `./isb.yaml`, else `./isb.yml`,
plus `./isb.override.yaml` when present). A `.env` next to the first file is
read unless `env_files` is given. `vars` win over the environment for `${VAR}`.
The result has `name`, `base_dir`, `files`, `file` (the resolved file, every
service's `container_name` filled in) and `services`. Then
`up(services=None, *, prune_devices=False, wait_ready=True)` returns
`(service, ApplyReport)` pairs, `plan(...)` returns `list[Plan]`,
`down(services=None, *, volumes=False)` deletes, and `sandbox("web")` returns a
handle with that service's exec defaults.

### Volumes and prune

`isb.volumes.list(pool=None)`, `get(name, pool=None)`,
`create(name, pool=None, *, config=None)` (returns `{"created", "pool"}`, a
no-op if it exists), `remove(name, pool=None)`. `isb.Volumes(client)` has the
same methods bound to one client. `await isb.prune(label, dry_run=True)` lists
(or, with `dry_run=False`, deletes) sandboxes whose `label` value is a host path
that no longer exists.

### Types

`SandboxInfo`, `ApplyReport`, `Plan`, `VolumeInfo`, `PruneResult`, `ExecOutput`
and `ExecEvent` are dataclasses. Plan actions are dicts tagged by `action`.
The spec types (`ComposeFile`, `SandboxSpec`, `VolumeSpec` = `str | VolumeMount`,
`PortSpec` = `str | PortMapping | ProxyPort`, `ExecSpec`, `ReadyCheck`,
`IdmapSpec`, `NamedVolumeSpec`, `MapOrList`, `Command`) are TypedDicts and
aliases in `isb._spec`, generated from the JSON Schema by
`scripts/gen_types.py`. `ExecDefaults` is the exec-defaults dict of the
protocol.


```sh
ISB_BIN=/path/to/isb python3 scripts/gen_types.py          # regenerate
ISB_BIN=/path/to/isb python3 scripts/gen_types.py --check  # fail if stale
```

## Errors

Every error is an `IsbError` with `code` (the protocol's stable code),
`message` and `data`:

| class | codes |
|---|---|
| `NotFoundError` | `not_found` |
| `AlreadyExistsError` | `already_exists` |
| `NotReadyError` | `not_ready` |
| `IsbTimeoutError` | `request_timeout`, `operation_timeout`, `exec_timeout` |
| `InvalidError` | `invalid`, `interpolation`, `parse` |
| `ConnectError` | `connect` (isb cannot reach incusd) |
| `ApiError` | `api`, `operation_failed` |
| `ProtocolError` | `protocol` (also unknown method), `bad_request`, a bad hello |
| `ProcessError` | `process`: the subprocess could not start or exited |
| `BinaryNotFoundError` | `binary_not_found` (a `ProcessError`) |

Other codes (`websocket`, `io`, `json`) raise `IsbError` itself.
`IsbTimeoutError` does not derive from the built-in `TimeoutError`.

## Development

Unit tests need only an isb binary with `rpc` (they use a socket that does not
exist, and fake servers for failure paths); integration tests need incusd and a
local image with a `dev` user at uid 1000 and python3 (`ISB_TEST_IMAGE`,
default `dev-base`). Both use the standard library's `unittest`, so no install
is needed:

```sh
# from the repository root
cargo build
ISB_BIN=target/debug/isb PYTHONPATH=sdk/python/src python3 -m unittest discover -s sdk/python/tests -v
ISB_INTEGRATION=1 ISB_BIN=target/debug/isb PYTHONPATH=sdk/python/src python3 -m unittest discover -s sdk/python/tests -v
```

Integration tests name everything `isb-test-py-<pid>-...`, label it
`isb-test=py`, and remove it afterwards.

Lint, type check and build (in `sdk/python`):

```sh
uv sync --group dev
uv run ruff check . && uv run ruff format --check .
uv run mypy
uv build                                      # pure wheel and sdist
ISB_WHEEL_BINARY=../../target/x86_64-unknown-linux-musl/release/isb \
ISB_WHEEL_PLAT=manylinux_2_17_x86_64.musllinux_1_2_x86_64 \
uv build --wheel                              # platform wheel with the binary
```

## License

MIT
