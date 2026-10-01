# @execution-associates/isb

TypeScript SDK for [isb](https://github.com/execution-associates/isb):
declarative incus sandboxes (containers and VMs). It is a thin client of
`isb rpc`, a line-delimited JSON protocol over the isb binary's stdin and
stdout ([docs/rpc.md](../../docs/rpc.md)). isb does the work; the SDK starts
it, sends requests and types the results.

- Bun first; Node 20 or later works too. ESM only.
- No runtime dependencies.
- Spec objects use the field names of the compose YAML exactly
  ([docs/spec.md](../../docs/spec.md)), so a spec can be copied between YAML
  and code. SDK options are camelCase.

## Install

```sh
bun add @execution-associates/isb
```

The package has optional dependencies `@execution-associates/isb-linux-x64`
and `@execution-associates/isb-linux-arm64`, each carrying the static isb
binary for that platform. The binary is found in this order:

1. `new Client({ isbBin })`
2. `$ISB_BIN`
3. the platform package (`bin/isb` inside it)
4. `isb` on `PATH`

isb needs access to the incus socket, which is root-equivalent on the host.

## Quickstart

```ts
import { PortBinding, Sandbox, Volume } from "@execution-associates/isb";

const sb = await Sandbox.connectOrCreate({
  container_name: "dev-web",
  image: "dev-base",
  cpus: 4,
  mem_limit: "8g",
  labels: { app: "web" },
  environment: { NODE_ENV: "development" },
  volumes: [
    Volume.bind("/srv/src", "/home/dev/src", { device: "src" }),
    Volume.named("dev-cache", "/home/dev/.cache", { owner: "dev" }),
    "/srv/ref:/home/dev/ref:ro", // the short form works too
  ],
  ports: [
    PortBinding.publish("5173-5193", 5173, { name: "vite" }), // first free port
    "127.0.0.1:8080:80",
  ],
  ready: ["running", "default_route", { user_exists: "dev" }],
  user: "dev",
  working_dir: "/home/dev/src",
});
console.log(sb.lastReport?.ports); // device name -> listen address in use

// Captured output. argv is never joined into a shell string.
const out = await sb.exec("printf", ["[%s]", "a b"]);
console.log(out.exitCode, out.stdoutText); // 0 "[a b]"

// Streaming output, piped stdin, signals.
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

await sb.remove({ force: true });
```

A compose file:

```ts
import { Project } from "@execution-associates/isb";

const project = await Project.load({
  files: ["isb.yaml"],
  vars: { WORKTREE: process.cwd() },
});
console.log(project.name, project.services);

for (const plan of await project.plan()) {
  console.log(plan.name, plan.actions);
}

// Like `isb up -d`: resolves once the sandboxes are ready. A service's
// `command` is for the foreground CLI `isb up` and is not run here.
const reports = await project.up({
  onProgress: (line) => console.error(line),
});

// A sandbox from the project carries its service's exec defaults.
const web = project.sandbox("web");
await web.exec(["bun", "install"]);

await project.down();
```

## API

### Client

`new Client({ isbBin?, socket?, project?, createTimeout?, cwd?, env?, spawner? })`
owns one long-lived `isb rpc` subprocess, started on the first request.
`socket`, `project` and `createTimeout` become the global flags `--socket`,
`--project` and `--create-timeout`. The client checks the server's hello and
refuses a protocol other than 1. Requests run concurrently and are matched by
id.

Every call takes an options object with an optional `client`. Without one, the
shared `defaultClient()` is used (`setDefaultClient()` replaces it).

- `await client.close()` closes the server's stdin and waits for it to exit.
  `await using client = new Client()` does the same at the end of the scope.
- An idle client does not keep the process alive, so a script that never calls
  `close()` still exits. With `spawner: "bun"` (Bun.spawn instead of
  `node:child_process`), it does until `close()`.
- If the subprocess dies, pending requests reject with `ProcessExitedError`
  (with its exit status and the end of its stderr) and the next request starts
  a new one.
- `client.request(method, params, onEvent?)` sends any rpc method directly.

### Sandbox

Static:

| method | does |
|---|---|
| `Sandbox.create(spec, { baseDir?, volumes?, waitReady?, onProgress? })` | create; `AlreadyExistsError` if it exists |
| `Sandbox.connectOrCreate(spec, { ..., pruneDevices? })` | create or reconcile (`isb up` for one sandbox); report on `sb.lastReport` |
| `Sandbox.plan(spec, { baseDir?, volumes?, pruneDevices? })` | what `connectOrCreate` would change: `Plan` |
| `Sandbox.resolve(spec, ...)` | the spec resolved against this host |
| `Sandbox.get(name)` | an existing sandbox; `NotFoundError` if missing |
| `Sandbox.listWith({ labels })`, `Sandbox.list()` | `SandboxInfo[]`; labels as `{ key: "value", other: null }` or `["key=value", "other"]` |
| `Sandbox.remove(name, { force? })` | delete; a running one needs `force` |

`spec` is a `SandboxSpec` (the `services.<service>` object of the compose
format) with `container_name` (the instance name) and `image` required.
`baseDir` anchors relative bind paths (default: the subprocess's working
directory). `volumes` holds named-volume definitions, like a compose file's
top-level `volumes:`: a mount's `source` names a definition's key, and the
incus volume is that definition's `name`, else the key itself (a mount with no
definition uses its `source` as the incus volume name). `onProgress` receives
lines such as `web: creating from dev-base`.

Instance: `info()`, `labels()`, `start()`, `stop({ force?, timeout? })`,
`restart()`, `waitReady({ ready?, readyTimeout? })`, `remove({ force? })`,
`addPort(port)` (a `ports` entry; returns the listen address in use), `removeDevice(name)`
(returns whether it was there), `exec(...)`, `execStream(...)`.

A Sandbox made by `create`, `connectOrCreate` or `project.sandbox()` remembers
the spec's exec defaults, `user`, `working_dir`, `exec.env` and `exec.login`
(sent with every exec, as `sb.execDefaults`), and its `ready` and
`ready_timeout` (used by `waitReady()` when not given). One from `get` has none.
`execDefaultsOf(spec)` computes the same defaults.

### exec

```ts
sb.exec(cmd, args?, {
  cwd?,
  user?,
  env?,
  login?,
  timeout?,
  stdin?,
  tty?,
  width?,
  height?,
})
```

`cmd` is the program with `args` as its arguments, or a whole argv array.
`timeout` is seconds or a duration string (`"90s"`, `"5m"`); past it the command
is killed and the call rejects with `IsbTimeoutError` (`code: "exec_timeout"`).
`stdin` is a string or bytes. Per-call options override the sandbox's exec
defaults.

It resolves to an `ExecOutput`: `exitCode`, `stdout` and `stderr` (bytes), and
the getters `stdoutText`, `stderrText` (UTF-8) and `success` (exit code 0). A
non-zero exit code is not an error.

`sb.execStream(cmd, args?, opts)` takes the same options, with `stdin` also
accepting `"piped"`, and resolves to an `ExecProcess` as soon as the request is
sent:

- `for await (const ev of proc)`: `{ kind: "stdout" | "stderr", data: Uint8Array }`
  in order per stream. One consumer. Output is buffered until read.
- `write(data)`, `closeStdin()`: with `stdin: "piped"`.
- `signal(15)` or `signal("SIGTERM")`, `resize(width, height)` (tty only).
- `wait()`: the exit code. `output()`: read the rest and return an `ExecOutput`.

With `tty: true` all output arrives as stdout.

### Builders

They return plain entries of a service's `volumes` and `ports` lists, in the
long form; the short strings (`"./src:/home/dev/src:ro"`, `"8080:80"`) can be
mixed in freely.

- `Volume.bind(hostPath, target, { readOnly?, device?, options? })`:
  `{type: "bind", source, target, ...}`.
- `Volume.named(name, target, { external?, owner?, readOnly?, pool?, device?, options? })`:
  `{type: "volume", source, target, ...}`, created if missing unless
  `external: true` (the volume must exist).
- `PortBinding.publish(published, target, { hostIp?, protocol?, name?, options? })`:
  docker's long port form. `hostIp` defaults to 127.0.0.1. A `published` range
  (`"5173-5223"`) with a single `target` takes the first free port in it.
- `PortBinding.host(listen, connect, { name?, search?, options? })`: an incus
  proxy listening on the host and connecting in the guest, with incus
  addresses (`5173`, `"0.0.0.0:5173"`, `"5353/udp"`, `"tcp:HOST:PORT"`).
  `search: N` returns a `publish` entry with the range `listen`..`listen+N`
  instead, and throws a `TypeError` when the addresses cannot be written that
  way (a port range, a unix socket, a connect host other than the default).
- `PortBinding.guest(listen, connect, { name?, options? })`: listen in the
  guest, connect on the host.

### Project (compose)

`Project.load({ files?, envFiles?, projectName?, vars?, client? })` loads,
interpolates and merges compose files without touching incus. Without `files`
it reads `isb.yaml` (or `isb.yml`) in the subprocess's working directory, plus
`isb.override.yaml` (or `.yml`) next to it; without `envFiles`, `.env` next to
the first file if present. `vars` win over the environment of the isb
subprocess and the env files. The project has
`name`, `baseDir`, `files`, `file` (the resolved `ComposeFile`) and `services`.

- `up({ services?, pruneDevices?, waitReady?, onProgress? })`: `[{ service, report }]`
- `plan({ services?, pruneDevices? })`: `Plan[]`
- `down({ services?, volumes?, onProgress? })`
- `spec(service)`, `sandbox(service)`

### Volumes and prune

- `volumes.list({ pool? })`, `volumes.get(name, { pool? })`,
  `volumes.create(name, { pool?, config? })` (`{ created, pool }`),
  `volumes.remove(name, { pool? })`
- `prune(label, { dryRun? })`: sandboxes whose `label` value is a host path that
  no longer exists. A dry run unless `dryRun: false`.

### Types

The spec types (`SandboxSpec`, `VolumeSpec` = string | `VolumeMount`,
`PortSpec` = string | `PortMapping` | `ProxyPort`, `MapOrList`, `Command`,
`ExecSpec`, `ReadyCheck`, `IdmapSpec`, `NamedVolumeSpec`, `ComposeFile`, ...)
are generated from `isb schema` into `src/spec.ts`. Results are `SandboxInfo`,
`ApplyReport`, `Plan` (with the `Action` union, tagged by `action`),
`VolumeInfo`, `PruneItem`; `ExecDefaults` is the exec-defaults shape the rpc
methods take. Result field names are as isb sends them
(snake_case).

## Errors

Every failure isb reports rejects with an `IsbError` carrying `code`,
`message` and `data` (docs/rpc.md lists them). Subclasses by code:

| class | codes |
|---|---|
| `NotFoundError` | `not_found` |
| `AlreadyExistsError` | `already_exists` |
| `NotReadyError` | `not_ready` |
| `IsbTimeoutError` | `request_timeout`, `operation_timeout`, `exec_timeout` |
| `InvalidError` | `invalid`, `interpolation`, `parse` |
| `ConnectError` | `connect` (incusd unreachable; `data.socket`) |
| `ProtocolError` | `protocol` (also an unknown method), `bad_request`, `websocket`, a bad hello |
| `ProcessExitedError` | `process_exited`: the subprocess could not start or died |
| `ClientClosedError` | `closed`: the client was closed |
| `IsbError` | anything else (`api`, `operation_failed`, `io`, `json`) |

## Development

Types are generated from the schema of the isb binary:

```sh
ISB_BIN=/path/to/isb bun run gen-types    # rewrites src/spec.ts
```

Dev dependencies (TypeScript, Biome, json-schema-to-typescript) are only for
typechecking, linting, type generation and building. The tests import from
`src/` only, so they run without `bun install`:

```sh
ISB_BIN=/path/to/isb bun test                                  # unit tests, no incus
ISB_INTEGRATION=1 ISB_BIN=/path/to/isb bun test                # plus integration tests
bun install && bun run typecheck && bun run lint && bun run build
```

Unit tests run the real `isb rpc` against a socket that does not exist, plus
small fake servers. Integration tests need incusd and a local image with a
`dev` user at uid 1000 (`ISB_TEST_IMAGE`, default `dev-base`). Everything they
create is named `isb-test-ts-<pid>-...` and is removed afterwards, pass or fail.

## License

MIT
