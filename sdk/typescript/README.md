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
  name: "dev-web",
  image: "dev-base",
  cpus: 4,
  memory: "8GiB",
  labels: { app: "web" },
  volumes: {
    "/home/dev/src": Volume.bind("/srv/src", { device: "src" }),
    "/home/dev/.cache": Volume.named("dev-cache", { owner: "dev" }),
  },
  ports: [
    PortBinding.host("5173", "5173", { name: "vite", search: 20 }),
  ],
  ready: ["running", "default_route", { user_exists: "dev" }],
  exec: { user: "dev", cwd: "/home/dev/src" },
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

`spec` is a `SandboxSpec` (the `sandboxes.<service>` object of the compose
format) with `name` and `image` required. `baseDir` anchors relative bind paths
(default: the subprocess's working directory). `volumes` holds named-volume
definitions, like a compose file's top-level `volumes:`. `onProgress` receives
lines such as `web: creating from dev-base`.

Instance: `info()`, `labels()`, `start()`, `stop({ force?, timeout? })`,
`restart()`, `waitReady({ ready?, readyTimeout? })`, `remove({ force? })`,
`addPort(port)` (returns the listen address in use), `removeDevice(name)`
(returns whether it was there), `exec(...)`, `execStream(...)`.

A Sandbox made by `create`, `connectOrCreate` or `project.sandbox()` remembers
the spec's `exec` defaults (sent with every exec) and its `ready` and
`ready_timeout` (used by `waitReady()` when not given). One from `get` has none.

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

They return plain spec objects.

- `Volume.bind(hostPath, { readonly?, device?, options? })`
- `Volume.named(name, { mode?, owner?, readonly?, pool?, device?, options? })`;
  `mode: NamedVolumeMode.Existing` sets `external: true` (the volume must exist),
  `NamedVolumeMode.EnsureExists` (default) creates it if missing.
- `PortBinding.host(listen, connect, { name?, search?, options? })`: listen on
  the host, connect in the guest.
- `PortBinding.guest(listen, connect, { name?, options? })`: listen in the
  guest, connect on the host.

### Project (compose)

`Project.load({ files?, envFiles?, projectName?, vars?, client? })` loads,
interpolates and merges compose files without touching incus. `vars` win over
the environment of the isb subprocess and the env files. The project has
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

The spec types (`SandboxSpec`, `VolumeSpec`, `PortSpec`, `ReadyCheck`,
`ExecDefaults`, `IdmapSpec`, `NamedVolumeSpec`, `ComposeFile`, ...) are
generated from `isb schema` into `src/spec.ts`. Results are `SandboxInfo`,
`ApplyReport`, `Plan` (with the `Action` union, tagged by `action`),
`VolumeInfo`, `PruneItem`. Result field names are as isb sends them
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
