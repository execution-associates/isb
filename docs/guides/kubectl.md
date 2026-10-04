---
title: isb for kubectl users
description: What you did with kubectl, as isb CLI commands and MCP tools: list and describe instances, exec, logs, copy files, scale, restart, events, top.
order: 14.5
---

If you manage a cluster with `kubectl`, you already know the shape of managing
an isb org: apps and stacks are the desired state, **instances** are what runs
(a pod is an instance, an app's replica is one), a controller keeps the one
equal to the other, and you look inside with `get`, `describe`, `logs` and
`exec`. isb's [MCP](agents.md) is meant to be that `kubectl` for an agent: the
same few verbs, one set of tools, in one org, with the same authorizer and the
[audit log](../operations/audit.md) behind every call.

An org's token (or the workspace token inside an org's
[workspace](../concepts/workspaces.md)) reaches that org and nothing else, as a
kubeconfig context does one cluster. The `isb` CLI works the same: on the
daemon's host it talks to the unix socket, anywhere else through
`ISB_URL`, `ISB_TOKEN` and `ISB_ORG`. Add `--org NAME` to act in another org.

## kubectl to isb

| kubectl | isb CLI | MCP tool |
|---|---|---|
| `get pods` | `isb instance ls [--app A] [--kind K]` | `instance_list` |
| `get deploy`, `get svc` | `isb app ls`, `isb stack ls` | `app_list`, `stack_list` |
| `describe pod` | `isb instance get NAME` | `instance_get` |
| `describe deploy` | `isb app show NAME`, `isb stack ps NAME` | `app_get`, `stack_status` |
| `apply -f`, `create` | `isb app create`, `isb stack deploy`, `isb up` | `app_create`, `app_deploy`, `stack_deploy` |
| `apply -f app.yaml`, `get -o yaml`, `diff` | the app page's **YAML** tab | `app_apply` (replaces the whole definition, refuses removals without `allow_removals`; `dry_run` first), `app_export` |
| `edit`, `patch`, `set image`, `set env` | `isb app update -f PATCH`, `isb app env-set` | `app_update`, `app_env_set` |
| `logs deploy/NAME` | `isb app logs NAME [--replica N] [-n 200] [--since 10m]` | `app_logs` |
| `logs -f`, `logs` for a compose service | `isb stack logs NAME SERVICE` | `stack_logs` |
| `exec POD -- CMD` | `isb app exec NAME -- CMD...`, `isb instance exec NAME -- CMD...` | `app_exec`, `instance_exec` |
| `exec -it POD -- sh` | the app page's **Terminal** tab, or `ssh` ([SSH and herdr](ssh.md)) | the terminal websocket |
| `cp POD:/path ./local`, `cp ./local POD:/path` | `isb cp INSTANCE:/path ./local`, `isb cp ./local INSTANCE:/path` | `instance_file_read`, `instance_file_write` |
| `scale deploy/NAME --replicas=N` | `isb app scale NAME N` | `app_scale` |
| `rollout restart deploy/NAME` | `isb app restart NAME [--wait]` | `app_restart` |
| `rollout status`, `rollout history` | `isb app deployments NAME`, `isb app deploy-log NAME` | `app_deployments`, `app_deployment_log` |
| `rollout undo deploy/NAME` | `isb app rollback NAME [ID]` | `app_rollback` |
| `delete pod NAME` | `isb instance restart NAME` | `instance_restart` |
| `delete deploy NAME` | `isb app rm NAME`, `isb stack rm NAME` | `app_delete`, `stack_remove` |
| `top pods` | `isb app top NAME` | `app_top`, `metrics_query` |
| `get events`, `get events --for` | `isb app events NAME` | `app_events`, `events`, `history_query` |
| `port-forward` | not offered; see below | `app_exec` with `curl`, or a service name from the workspace |
| `get secret`, `create secret` | `isb secret get`, `isb secret set` | `secret_get`, `secret_set` |
| `get ns`, `config use-context` | `isb org ls`, `--org NAME` | `org_list`, the endpoint `/orgs/NAME/mcp` |

## Look: list and describe

`instance_list` is `get pods -o wide` for the whole org. One row per instance
isb manages, whatever it is:

```text
$ isb instance ls
NAME                          KIND      OWNER  SLOT  STATUS   HEALTH   ROTATION  IP              RESTARTS  AGE  CPU   MEMORY
demo-production-cache-1-4d93  database  cache  1     Running  healthy  yes       10.175.180.57   0         1m   0.4%  3Mi
demo-production-web-1-b737    app       web    1     Running  none     yes       10.175.180.71   0         2m   0.0%  756Ki
demo-production-web-2-98d1    app       web    2     Running  none     yes       10.175.180.246  0         1m   0.0%  756Ki
scratch1                      sandbox   -      -     Running  none     -         10.175.180.89   -         1m   0.0%  29Mi
```

`kind` says what the instance is to isb: `app` (a replica of an app; a
preview's replicas too), `database`, `stack` (a replica of a compose
service), `tunnel` (an org's cloudflared), `workspace`, `sandbox` and `build`.
Filter with `app`, `stack`, `service`, `kind` and `status`. **Rotation** is
whether the load balancer sends it traffic; a replica that is running but
unhealthy is out of rotation. **Health** is `none` for an app without a health
check (it is judged by its process alone).

`instance_get` is `describe pod`: the same row plus the image and revision,
limits, the **names** of its environment variables (never their values),
volumes and devices, ports, the domains its service serves and whether
traffic reaches this replica, the last health probe, the files isb itself
delivers into it, its labels, and its recent history (the controller's events,
incus lifecycle events and restarts):

```sh
isb instance get demo-production-web-1-b737
isb instance get demo-production-web-1-b737 --json    # everything, for a script
```

`app_top` is `top pods`: per replica CPU (percent of one core), memory, disk
and network counters now, with the totals next to the app's limits. For the
history, [`metrics_query`](../operations/metrics.md) has it. `app_events` is
`get events --for`: what the controller and the deploys said about one app,
newest last.

## Logs

```sh
isb app logs web                     # every replica, 200 lines each, prefixed by slot
isb app logs web --replica 2 -n 50 --since 10m
```

`app_logs` reads the supervised command's journal (or an OCI image's console)
by **app name**, so an agent need not know the stack or service; it is
`stack_logs` underneath. `since` keeps the lines newer than a duration and
works on system images; an OCI image's console log has no timestamps, so there
the whole tail is shown and the answer says so. A replica that was replaced
takes its logs with it: there is no `--previous`. `app_events` and
`history_query` say what happened to it.

A replica that fails to come up (a crash loop: the app exits within its
monitor period, or never answers its healthcheck) is deleted and made again,
so there is nothing to read from it later. isb reads its output before it
deletes it, puts the last lines in the failure message (the deployment log,
the app's status message) and keeps them: while the app is not converged,
`app_logs` answers `last_failed_attempt` with the instance, the reason and
the output, and `isb app logs` prints it after the live replicas' logs.

The CLI's old `isb app logs` (a deployment's build and rollout log) is now
`isb app deploy-log NAME [ID] [-f]`.

## Run a command

```sh
isb app exec web -- cat /etc/os-release
isb app exec web --replica 2 -w /app -e DEBUG=1 -- sh -c 'ls | head'
echo 'select 1' | isb app exec db -i -- psql -U app
isb instance exec workspace -- git -C ~/src status
```

`app_exec` runs argv in one replica of an app: by default a running one,
preferring replicas that are healthy and in rotation; `replica` (the slot) or
`instance` choose one. `instance_exec` runs in any running instance of the org:
a replica, a database, a sandbox, the workspace (as its user, in its home). It
is not interactive and not a shell: argv is a list, so use `["sh", "-c",
"..."]` for pipes. What it takes and gives:

| | |
|---|---|
| `argv`, `cwd`, `user`, `env`, `stdin` | as `sandbox_exec`; `stdin` is text of at most 1 MiB |
| `timeout` | default 60s, at most 15 minutes; a command that runs out is killed and answered with `timed_out: true` and the output so far |
| result | `exit_code`, `stdout`, `stderr`, `timed_out`, `duration_ms`, `replica`/`instance` |
| output | each stream keeps its **last 1 MiB**; `stdout_truncated`, `stderr_truncated` and `truncated` say so, and `stdout_bytes`/`stderr_bytes` how much there was |

`isb app exec` prints the output, exits with the command's status (124 on a
timeout), and says on stderr when the output was cut. An image built `FROM
scratch` (traefik/whoami, say) has no shell or tools: run its own binary
(`isb app exec whoami -- /whoami --help`), or [`isb cp`](#copy-small-files).

**Who may:** members, admins and owners of the org; a viewer is refused, as
are tokens scoped `read` or `deploy`, and everything is gone when the operator
runs `--deny-tools app_exec,instance_exec` (the web terminal is admitted as
`sandbox_exec`, which has its own switch). A workspace token is an org admin,
so an agent in the workspace may. Every call, refused ones too, is in the
audit log with the **argv** (up to 64 arguments), the names of the `env`
variables and the size of `stdin`, never their values. Treat argv as logged:
pass a password in `env` or on `stdin`, not in the command line.

On an org placed on [another server](servers.md) the call is forwarded to it
like any other.

## Copy small files

```sh
isb cp demo-production-web-1-b737:/etc/app/config.yml ./config.yml
isb cp ./config.yml demo-production-web-1-b737:/etc/app/config.yml
```

`instance_file_read` returns one file, running instance or stopped, of at
most **4 MiB**: text as `utf8`, anything else as `base64` (`encoding` asks for
one). `instance_file_write` replaces one with `content` (`utf8` or `base64`) of
at most **2 MiB**, owned by `uid` and `gid` (default root) with `mode` (default
`0644`), creating missing directories unless `parents` is false. For bigger
files, run `tar`, `head`, `tail` or `split` with `instance_exec`, or use `scp`
([SSH and herdr](ssh.md)).

What they refuse: paths that are not absolute or contain `..`; the kernel's
`/proc`, `/sys` and `/dev`; a workspace's token (`/run/isb/token`), for reading
and writing; and, for writing, everything isb itself delivers: `/run/isb`,
`/run/secrets`, `/etc/isb`, and the files of an app's `files` or a stack's
secrets, which come from org secrets and are changed there. A file can hold
secrets, so reading one is a *secret read*: members and up only, always in the
audit log (path and size, never the content). Writing records the path and the
size.

## Scale and restart

```sh
isb app scale web 4
isb app restart web --wait      # a rolling replace, same settings
isb instance restart demo-production-web-1-b737 --wait
```

`app_scale` sets the replica count (0 stops the app without removing it) and
saves it as the app's `replicas` setting too, so the next deploy keeps it.
`app_restart` is `rollout restart`: the replicas are replaced one by one by
fresh instances of the same settings, in the order the app's `update_config`
says (stop-first by default; `order: start-first` keeps it serving); it also
picks up a moved image tag. `instance_restart` is `delete pod`: a replica is
deleted and the controller makes its replacement for the slot (`wait` blocks
until it runs); a sandbox is restarted in place. The workspace has
`workspace_restart`. All three need a member, emit an event
([`app_events`](#look-list-and-describe)) and are in the audit log; a token
scoped `deploy` may scale and restart.

## What isb does differently

- **No port-forward.** An agent that needs to reach an app runs `curl` (or the
  client it needs) inside an instance with `app_exec`, or, from the org's
  workspace, uses the instance's address from `instance_list`. A person opens
  the app's domain, or a workspace port's
  [preview](../concepts/workspaces.md).
- **Desired state is a compose file or an app, not a manifest per object.**
  `app_update` takes a JSON merge patch (`null` clears a setting) where
  `kubectl patch` does; there is no `edit`, so read with `app_get`, change,
  write with `app_update`, then `app_deploy`.
- **An org is the namespace and the context.** Every tool takes `org` and an
  org-bound endpoint fixes it; there is no cluster-wide view for an org token.
- **Instances are incus containers and VMs**, not pods: one process tree per
  instance, an address of its own on the org's bridge, state kept in its
  root disk and named volumes. Deleting a replica loses what was written
  outside a volume.
