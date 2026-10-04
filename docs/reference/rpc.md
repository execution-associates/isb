---
title: The rpc protocol
description: isb rpc, the line-delimited JSON protocol the Python and TypeScript SDKs speak, for driving isb from any language.
order: 7
nav_title: rpc protocol
---

`isb rpc` serves the SDK protocol on stdin/stdout: line-delimited JSON, one
object per line, UTF-8. It is how the Python and TypeScript SDKs drive isb, and
any other language can use it the same way. Protocol version: **1**. To use
isb from Python, TypeScript or Rust, start with [isb from code](../guides/sdk.md).

Global flags apply: `isb --socket PATH --project P --create-timeout 10m rpc`.
The server inherits its working directory and environment from whoever starts
it; relative paths resolve against that directory.

## Framing

When it starts, the server writes one hello line:

```json
{"isb": "1.0.0", "protocol": 1}
```

A client checks `protocol` and refuses versions it does not know.

Requests (client to server):

```json
{"id": 1, "method": "sandbox.get", "params": {"name": "web"}}
```

`id` is any JSON number or string chosen by the client, unique among its
in-flight requests. `params` is an object (omit it or pass `{}` when empty).

Each request gets exactly one final reply with the same `id`, either:

```json
{"id": 1, "result": {...}}
{"id": 1, "error": {"code": "not_found", "message": "sandbox web not found", "data": {...}}}
```

Before its final reply, a request may send any number of events:

```json
{"id": 1, "event": "progress", "data": "web: creating from dev-base"}
{"id": 2, "event": "stdout", "data": "aGVsbG8K"}
```

Requests run concurrently, so replies and events of different requests
interleave; match them by `id`. A line that is not a request gets an error with
`"id": null` and code `bad_request`.

When the client closes the server's stdin, the server finishes in-flight
requests and exits. Commands still running under `sandbox.exec` lose their
control connection when it exits, and incus kills them.

## Errors

`code` is stable; `message` is for humans; `data` is present for some codes.

| code | meaning | data |
|---|---|---|
| `connect` | cannot reach incusd | `socket` |
| `request_timeout` | one request to incusd took too long | `method`, `path`, `timeout_secs` |
| `api` | incusd returned an error | `status`, `method`, `path` |
| `not_found` | sandbox, volume, device or running exec missing (also incusd 404) | sometimes `status`, `method`, `path` |
| `already_exists` | name taken (also incusd 409) | sometimes `status`, ... |
| `operation_timeout` | an incus operation ran past its deadline | `step`, `operation`, `cancelled`, `waited_secs` |
| `operation_failed` | an incus operation failed | `step` |
| `not_ready` | a readiness check did not pass in time, or the instance stopped | `sandbox`, `check`, `detail`, `waited_secs` |
| `exec_timeout` | an exec with `timeout` ran past it and was killed | `timeout_secs` |
| `invalid` | bad params, spec or argument | |
| `interpolation` | `${VAR}` could not be resolved (inside a compose file this arrives as `parse`, naming the file) | |
| `parse` | a compose file could not be read or parsed | `path` |
| `websocket`, `protocol`, `io`, `json` | lower-level failures; `protocol` also means unknown method | |
| `bad_request` | the line was not a request | |

## Types

- **spec**: a sandbox spec, exactly the `services.<service>` object of the
  compose format ([isb.yaml](compose.md#servicesservice)), with `container_name` required.
  Its [`egress`](compose.md#egress) field (`"none"`, a list of `host[:port]`,
  or `{"allow": [...], "secrets": [...]}`) confines the sandbox's network
  ([Sandbox egress and secrets](../guides/egress.md)): `sandbox.create` and
  `sandbox.ensure` make its bridge and ACL, and `sandbox.remove` deletes
  them. The proxy that enforces it is `isb serve`'s. The JSON Schema from `isb schema`
  (or method `schema`) describes it as the `SandboxSpec` definition.
- **info**: a sandbox as listed:
  `{name, status, type, labels, config, devices, profiles, created_at, description}`.
  `labels` are the `user.*` keys without the prefix; `devices` are
  instance-local devices, each a map of string properties.
- **report**: what an ensure did:
  `{name, created, applied: [action...], ports: {device: listen}, restart_needed: [key...]}`.
- **plan**: `{name, status (null if missing), actions: [action...]}`, where each
  action is an object tagged by `action` (`create_volume`, `create_instance`,
  `set_config`, `add_device`, `replace_device`, `remove_device`,
  `start_instance`, `add_port`, `fix_owner`, `note`).
- Durations are strings: `"90s"`, `"5m"`, `"1500ms"`, `"30"` (seconds).
- Binary data (exec output and stdin) is standard base64 with padding.

## Methods

Every method accepts an optional `project` (incus project) param.

### General

| method | params | result |
|---|---|---|
| `version` | | `{isb, protocol}` |
| `schema` | | the compose-format JSON Schema |

### Sandboxes

| method | params | result |
|---|---|---|
| `sandbox.create` | `spec`, `base_dir?`, `volumes?`, `wait_ready?` (true) | info. Fails `already_exists` if present. Sends `progress` events. |
| `sandbox.ensure` | `spec`, `base_dir?`, `volumes?`, `wait_ready?` (true), `prune_devices?` | `{info, report}`: creates or reconciles; a correct device is never touched. Sends `progress`. |
| `sandbox.plan` | `spec`, `base_dir?`, `volumes?`, `prune_devices?` | plan |
| `sandbox.resolve` | `spec`, `base_dir?`, `volumes?` | the spec resolved against this host (config keys, devices, pool, readiness) |
| `sandbox.get` | `name` | info |
| `sandbox.list` | `labels?`: `["key", "key=value"]` | [info], filtered by all labels |
| `sandbox.remove` | `name`, `force?` | null. A running sandbox needs `force`. |
| `sandbox.start` | `name` | null, once running |
| `sandbox.stop` | `name`, `force?`, `timeout?` (`"30s"`) | null |
| `sandbox.restart` | `name` | null |
| `sandbox.wait_ready` | `name`, `ready?` (checks, default `["running"]`), `ready_timeout?` (`"60s"`), `exec?` (exec defaults `{user, cwd, env, login}`, for `path_writable`) | null |
| `sandbox.add_port` | `name`, `port` (a `ports` entry) | `{listen}`: the address in use |
| `sandbox.remove_device` | `name`, `device` | `{removed}` |

`base_dir` anchors relative bind paths (default: the server's working
directory). `volumes` are named-volume definitions, as in a compose file's
top-level `volumes:`: a mount's `source` is looked up there, and the
definition's `name` (else the key) is the incus volume name. With no
definitions, the `source` is the incus volume name.

### Exec

`sandbox.exec` params:

| param | type | |
|---|---|---|
| `name` | string | sandbox |
| `argv` | [string] | program and arguments, never joined into a shell string |
| `defaults` | object | exec defaults from the spec (`user`, `cwd`, `env`, `login`); the params below override them |
| `cwd`, `user` | string | `user` is a name, `uid` or `uid:gid` |
| `env` | {string: string} | |
| `login` | bool | run via the user's login shell |
| `tty` | bool | pseudo-terminal; output then all arrives as `stdout` |
| `width`, `height` | int | tty size (default 80x24) |
| `timeout` | duration | kill after this long; none by default |
| `stdin` | `"null"` (default), `"piped"`, or `{"data": base64}` | |
| `stream` | bool | send output as events instead of in the result |

Without `stream`, the result is `{exit_code, stdout, stderr}` (base64). With
`stream`, output arrives as `stdout`/`stderr` events (base64 chunks, in order
per stream) and the result is `{exit_code}`.

While an exec runs, these requests drive it, with `exec` set to the exec
request's `id`:

| method | params | |
|---|---|---|
| `exec.write` | `exec`, `data` (base64) | write to stdin (`stdin: "piped"` only) |
| `exec.close_stdin` | `exec` | EOF on stdin |
| `exec.signal` | `exec`, `signal` (number, e.g. 15) | |
| `exec.resize` | `exec`, `width`, `height` | tty only |

They may be sent right behind the `sandbox.exec` request, without waiting for
anything: calls that arrive before the command has started are queued and
applied in order once it runs. They fail with `not_found` only when no exec
with that id is running (it finished, or never existed).

### Volumes

| method | params | result |
|---|---|---|
| `volume.list` | `pool?` | [volume] (all pools when omitted) |
| `volume.get` | `name`, `pool?` | volume |
| `volume.create` | `name`, `pool?`, `config?` | `{created, pool}`; no-op if it exists |
| `volume.remove` | `name`, `pool?` | null; refused while in use |

A volume is `{name, pool, content_type, config, used_by}`. `pool` defaults to
`auto` (incus-zfs, else default, else the first pool).

### Prune

| method | params | result |
|---|---|---|
| `prune` | `label`, `dry_run?` (true) | `[{name, path, deleted}]` for sandboxes whose `label` value is a host path that no longer exists |

### Compose

All take `files` (default `./isb.yaml` plus `./isb.override.yaml` if present),
`env_files?` (default `.env` next to the first file, if present), `project_name?`, and
`vars?` (a map that wins over the server's environment for `${VAR}`).

| method | extra params | result |
|---|---|---|
| `compose.load` | | `{name, base_dir, files, file}` (the resolved file, with every `container_name` and volume `name` filled in) |
| `compose.up` | `services?`, `wait_ready?` (true), `prune_devices?` | `[{service, report}]`; sends `progress` |
| `compose.plan` | `services?`, `prune_devices?` | [plan] |
| `compose.down` | `services?`, `volumes?` | null; sends `progress` |

## Example session

```text
<- {"isb":"1.0.0","protocol":1}
-> {"id":1,"method":"sandbox.ensure","params":{"spec":{"name":"web","image":"dev-base","cpus":2}}}
<- {"id":1,"event":"progress","data":"web: creating from dev-base"}
<- {"id":1,"event":"progress","data":"web: starting"}
<- {"id":1,"result":{"info":{"name":"web","status":"Running",...},"report":{...}}}
-> {"id":2,"method":"sandbox.exec","params":{"name":"web","argv":["cat"],"stdin":"piped","stream":true}}
-> {"id":3,"method":"exec.write","params":{"exec":2,"data":"aGkK"}}
<- {"id":3,"result":null}
-> {"id":4,"method":"exec.close_stdin","params":{"exec":2}}
<- {"id":4,"result":null}
<- {"id":2,"event":"stdout","data":"aGkK"}
<- {"id":2,"result":{"exit_code":0}}
```
