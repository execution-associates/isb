---
title: Stacks
description: Long-running services on the isb serve daemon, with replicas, health checks, a load balancer, rolling updates and rollbacks.
order: 4
---

`isb up` runs sandboxes for as long as you hold them. `isb stack deploy` hands
a compose file to the `isb serve` daemon, which keeps it running the way
docker swarm keeps a stack running, on one host: replicas, health checks, a
load balancer, rolling updates and rollbacks. Use a stack when a service must
run for months without anyone watching it.

```yaml
# isb.yaml
secrets:
  db_password: {environment: DB_PASSWORD}
services:
  api:
    image: dev-base
    user: dev
    working_dir: /home/dev/app
    volumes: ["./app:/home/dev/app:ro"]
    command: [node, server.js]
    environment: {PORT: "8080"}
    secrets: [{source: db_password, uid: 1000}]
    ports: ["127.0.0.1:8080:8080"]          # balanced over healthy replicas
    healthcheck:
      test: [CMD, curl, -fsS, http://127.0.0.1:8080/health]
      interval: 10s
    depends_on:
      db: {condition: service_healthy}
    deploy:
      replicas: 3
      update_config: {order: start-first}   # no gap during a rollout
  db:
    image: docker:postgres:17
    environment: {POSTGRES_PASSWORD_FILE: /run/secrets/db_password}
    secrets: [db_password]
    volumes: ["pgdata:/var/lib/postgresql/data"]
    healthcheck: {test: [CMD, pg_isready, -U, postgres], interval: 5s}
volumes:
  pgdata: {}
```

```console
$ isb serve install                 # once: the daemon as a systemd user service
$ DB_PASSWORD=... isb stack deploy  # stack name: the project name, or give one
db: create (rev 3f2a91c0, 1 replicas)
api: create (rev 8c11d0e4, 3 replicas)
SERVICE  STATE      REPLICAS  INSTANCE        STATUS   HEALTH   IP
api      converged  3/3       app-api-1-5e0a  RUNNING  healthy  10.180.0.31
                              app-api-2-c41b  RUNNING  healthy  10.180.0.43
                              app-api-3-77d2  RUNNING  healthy  10.180.0.58
db       converged  1/1       app-db-1-0b9e   RUNNING  healthy  10.180.0.12
api: 127.0.0.1:8080 -> :8080 (3 backends)
```

## What keeps it running

Three layers, each working without the ones above it:

1. **The guest supervises the app.** On a system image, `command` is a
   systemd unit with `Restart=`; on an OCI image it is the instance's init,
   which incus restarts (`boot.autorestart`). A crashed app comes back with
   nothing else involved.
2. **incus starts the instances with the host** (`boot.autostart`), and the
   unit restores the app's secrets before every start, so a reboot needs no
   isb.
3. **The daemon heals what the guest cannot see:** it runs each
   `healthcheck`, takes failing replicas out of the load balancer and
   restarts their app (and replaces the instance after three restarts that
   did not help), starts stopped instances, recreates deleted ones, and puts
   back replicas until there are `deploy.replicas`.

Stopping or upgrading the daemon never stops an app. What stops with it is
the load balancer: published ports close until it starts again (systemd
restarts it within seconds), and it resumes every stack from its state
directory without touching healthy instances.

## Replicas and names

Each service has `deploy.replicas` slots. A slot holds one instance named
`<stack>-<service>-<slot>-<id>`, with `id` new for every instance, labelled
`user.isb.stack`, `user.isb.service`, `user.isb.slot` and `user.isb.rev`. A
`container_name` in the file is ignored. Named volumes are `<stack>_<key>`
and are shared by every replica, as docker volumes are on one host: a service
that must own its volume should stay at one replica with `stop-first`
updates.

## Revisions and rollouts

A service's revision is a hash of everything that shapes its instances: the
service spec (minus replica count, rollout settings, dependencies and
published ports), its secrets' references (store name and version, so `isb
secret set` rolls it; see [Secrets](../guides/secrets.md#stacks)), and its
named volumes' definitions. Deploying a file whose revision changed replaces
that service's instances, in batches of `update_config.parallelism`:

- `stop-first` (default): take the old instance out of rotation, let its
  connections drain (up to 10 s), delete it, create the new one, wait for it.
- `start-first`: create the new one alongside, wait until it is healthy, put
  it in rotation, then drain and delete the old one. No moment without the
  slot serving.

"Wait for it" means: readiness checks pass, then the healthcheck passes (or,
with none, the app's unit is active), within `start_period + interval x
retries + 30s` (at least a minute); then it must stay healthy for
`update_config.monitor` (5 s). A failure deletes the new instance and applies
`failure_action`: `pause` (default) stops the rollout and leaves the service
as it is until the next deploy, `rollback` redeploys the previous version of
the whole stack, `continue` carries on.

Changing only `replicas` scales without replacing anything. `isb stack
redeploy STACK SERVICE` replaces a service's instances anyway, to pick up a
moved tag (`docker:app:latest`) or changed bind-mounted files. `isb stack
rollback STACK` goes back to the previous deployment; a second rollback
undoes the first.

## The load balancer

Every published host port (`ports`) is served by the daemon, not by an incus
proxy device: it listens on the host address and spreads connections over
the replicas' bridge addresses, least-connections first. Only replicas that
are healthy (or, without a healthcheck, whose app is running) receive
traffic. A replica whose connection fails is skipped for a backoff and the
client is tried on the next one, and a replica taken out of rotation keeps
its open connections until they end.

Limits: TCP only (a UDP published port is an error in a stack), single ports
(no ranges), and backends see the daemon's address, not the client's.
Guest-bound ports (`bind: guest`) stay per-instance proxy devices.

## Domains

A service's `domains:` put it on a public hostname over HTTP(S), when the
daemon runs its ingress (`isb serve --ingress-http/--ingress-https`, or
`--ingress-tunnels`):

```yaml
services:
  web:
    domains:
      - {host: shop.example.com, port: 8080}     # https by default, with a certificate
      - {host: auto, port: 8080}                 # web-shop-acme.203-0-113-7.sslip.io
```

The ingress (Caddy, run by the daemon) sends each request to a replica in
rotation, the same set the load balancer and service names use, so rollouts
are as gapless as for published ports: a replica leaves the routes, its
requests in flight finish, and only then is it stopped. Certificates come
from Let's Encrypt. A hostname another org serves is refused at deploy. See
[Domains and ingress](../guides/domains.md).

## Service discovery

Replica names change with every rollout, so a stack in an org gets stable
names for its services, resolvable from anything in the same org:

- `<service>.<stack>.<org>.isb`, e.g. `db.shop.acme.isb`;
- `<service>.<stack>`, e.g. `db.shop`, the same records.

A name resolves to the IPv4 address of every replica that is in rotation
(healthy, or running when there is no healthcheck): DNS round-robin, like
swarm's `dnsrr` endpoint mode, with a TTL of 0. A one-replica service
resolves to its one instance. The records follow the load balancer's view: a
replica leaves the name when it leaves rotation (unhealthy, or drained
during a rollout) and joins when it enters, so a `start-first` rollout always
has an address to give out. A service with no replica in rotation has no
record (NXDOMAIN), and removing the stack removes its names. A service name
with characters a DNS label cannot hold is spelled as in instance names
(`my_db` is `my-db`).

The daemon writes each service's records into the org's hosts directory,
which the org's dnsmasq watches; a change is served within milliseconds
(about 25 ms measured on a test host, the time of a `dig`). See
[Service names](orgs.md#service-names) for the mechanism and its one-time
host setup. While the daemon is down the last records stay as they were, and
a restarted daemon keeps them until a replica of the service is back in
rotation.

`isb up` (no daemon) gets no service names. Its services reach each
other by instance name, `<project>-<service>` (the default
`container_name`), which incus' DNS serves as `<project>-<service>.<org>.isb`
in an org (`.incus` for plain sandboxes in incus' `default` project) and through the search domain
as the bare name.

## Health and restarts

A replica's probe runs every `start_interval` until its first result, then
every `interval`. Failures during `start_period` do not count. After
`retries` consecutive failures it is unhealthy: out of rotation, and its app
is restarted (the unit, or the OCI instance). `deploy.restart_policy` bounds
this: `condition: none` never restarts, and `max_attempts` within `window`
stops restarting once spent (the status says so). Under a stack, `restart`
in the file is ignored, as swarm ignores it: the app is always supervised,
with `restart_policy` deciding the unit's `Restart=`.

## Dependencies

A service whose `depends_on` is not met waits (state `waiting`):
`service_started` needs a running replica of the dependency,
`service_healthy` a healthy one. Once met, the service is reconciled
independently; a dependency that later fails does not stop it.

## Status

`isb stack ps STACK` (or the `stack_status` tool) shows, per service, its
state:

| State | Meaning |
|---|---|
| `starting` | Not looked at yet. |
| `waiting` | `depends_on` not met. |
| `updating` | Creating or replacing instances. |
| `converged` | Every slot has a current instance in rotation. |
| `paused` | A rollout failed with `failure_action: pause`; the message says why. Deploy again to retry. |
| `failing` | Something is wrong that the daemon keeps retrying (an image that will not pull, a replica that stays unhealthy). |

and per replica its status, health, address, whether it is in rotation, its
restarts and its last probe output. `isb stack logs STACK SERVICE` shows each
replica's recent output.

## Commands

```text
isb stack deploy [NAME] [-f FILE...] [-d] [--timeout 10m]   deploy or update; waits unless -d
isb stack ls [--json]
isb stack ps NAME [--json]
isb stack logs NAME SERVICE [--slot N] [-n 100]
isb stack scale NAME SERVICE=N...
isb stack redeploy NAME SERVICE
isb stack rollback NAME
isb stack config NAME
isb stack rm NAME [--volumes]
```

`deploy` reads the file where you run it, exactly as `isb up` would (`.env`,
`--env-file`, `${VAR}`), reads `file:` and `environment:` secrets from your
files and environment, and sends the result to the daemon, which stores those
values in the org's store as `<stack>_<key>` and reads `external`, `age` and
`driver` secrets itself ([Secrets](../guides/secrets.md#stacks)); relative
bind paths stay relative to the file. It exits 0 once every service is
converged, 1 if one paused or is failing.

The CLI reaches the daemon over its unix socket (`$ISB_SERVE_SOCKET`, else
`$XDG_RUNTIME_DIR/isb/serve.sock`). Setting up the daemon is in
[Setting up a host](../operations/host-setup.md); remote people and agents
reach the same operations as [MCP tools](../reference/mcp-tools.md)
([Agents and MCP](../guides/agents.md)). `isb tui` shows all of it live
([isb tui](../reference/tui.md)).
