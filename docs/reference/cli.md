---
title: CLI reference
description: Every isb command and flag, the global flags, where each command sends its work, and the exit codes.
order: 1
nav_title: CLI
---

One binary, `isb`, does everything: it reconciles sandboxes on incus
directly, talks to the `isb serve` daemon for stacks, apps and the rest of
the platform, and edits the daemon's identity store and audit log on the
host. This page lists every command. `isb --help` and `isb COMMAND --help`
print the same flags with their one-line help.

```sh
isb --help            # every command
isb COMMAND --help    # one command's flags
isb --version
```

## Global flags

These go before or after the subcommand.

| Flag | Environment | Default | |
|---|---|---|---|
| `--socket PATH` | `INCUS_SOCKET` | `$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`; on macOS `~/.isb/machine/isb/incus.sock` | incusd's unix socket |
| `--project NAME` | `INCUS_PROJECT` | `default`, or the compose file's `incus_project` | the incus project to work in |
| `--org ORG` | `ISB_ORG` | the `default` org for platform commands; none for `create` and `up` | the org to work in: its incus project, `isb-<org>`, and the `org` of every daemon call. Without it, `isb create` and `isb up` make plain sandboxes in incus' `default` project, outside every org; `--org default` is the default org, `isb-default` |
| `--env-file FILE` | | `.env` next to the first compose file | dotenv file(s) for `${VAR}` (repeatable; the environment wins) |
| `-P`, `--project-name NAME` | | the file's `name`, else its directory | the compose project name |
| `--create-timeout D` | | `10m` | deadline for creating an instance (`20m` for slow image downloads) |
| `-q`, `--quiet` | | | no progress lines on stderr |

`-f FILE` (`--file`, repeatable) picks the compose files. It is accepted
before the subcommand and after the compose-aware ones: `up`, `down`,
`plan`, `config`, `ps`, `inspect`, `exec`, `logs` and `stack deploy`
(`isb -f a.yaml up -f b.yaml` loads `a.yaml`, then `b.yaml`). Without
`-f`, isb reads `./isb.yaml` (or `isb.yml`) with `./isb.override.yaml` (or
`isb.override.yml`) merged over it when present, and never a parent
directory's file; with none, the command says so and names the directory.
`isb up` prints the file it uses (`using /path/isb.yaml`). See
[Files and validation](compose.md#files-and-validation).

## Where commands send their work

- **Sandbox and compose commands** (`create`, `up`, `exec`, `volume create`,
  `port`, ...) talk to incusd over its socket. Access to that socket is
  root-equivalent on the host.
- **Platform commands** (`stack`, `app`, `project`, `secret`, `db`,
  `backup`, `job`, `build`, `template`, `notify`, `org` with a server or VM,
  `server`, `workspace`, `volume snapshot|show|restore`, `ingress`,
  `registry ls|gc`) call tools on the `isb serve` daemon over its unix socket
  (`$ISB_SERVE_SOCKET`, else `$XDG_RUNTIME_DIR/isb/serve.sock`, else
  `<tmp>/isb-<uid>/serve.sock`; on macOS the machine's
  `~/.isb/machine/isb/serve.sock`). Where there is no such socket and
  `ISB_URL` is set (inside an org's [workspace](../concepts/workspaces.md)),
  they call `$ISB_URL` with the API token in `ISB_TOKEN`, in the org from
  `--org`, else `$ISB_ORG`.
- **Identity and log commands** (`user`, `invite`, `token`, `audit`,
  `history`, and `key` without `--url`) open the daemon's files under its
  state directory directly (`--state-dir`, `ISB_SERVE_STATE_DIR`, default
  `$XDG_STATE_HOME/isb`), so they work before any user exists and while the
  daemon is down. Run them as the daemon's user.

## Exit codes

| Code | When |
|---|---|
| 0 | success |
| 1 | an error |
| 2 | `isb plan --exit-code` found changes |
| 2 | `isb update --check` found a newer release |
| the command's own | `isb exec`, and a foreground `isb up` (the first failing `command`) |
| 125 | `isb exec`: isb itself failed (no such sandbox, incusd unreachable), not the command |
| 128 + N | a foreground `isb up` stopped by signal N (130 for Ctrl-C) |
| 129 | a foreground `isb up` whose starting process went away |
| 141 | a foreground `isb up` whose stdout was closed |

## Sandboxes

```text
isb create NAME -i IMAGE [--vm] [--cpus N | --cpuset-cpus SET] [-m MEM] [-s POOL]
                [--idmap auto|none|always|RAW] [--privileged true|false]
                [-l KEY=VALUE]... [-e KEY=VALUE]... [-v SRC:GUEST[:OPTS]]... [-p PORT]...
                [--ready CHECK]... [--ready-timeout D] [-c KEY=VALUE]... [--profile P]...
                [--egress HOST[:PORT]|none]... [--secret NAME[=SECRET]@HOSTS]...
                [--ensure] [--no-ready]
isb start NAME...                      start, and wait until running
isb stop NAME... [-f] [-t 30s]         clean shutdown (or kill with -f) within -t
isb restart NAME...
isb rm NAME... [-f]                    delete (-f stops a running one first); aliases remove, delete
isb ls [-l KEY[=VALUE]]... [--json]    list, filtered by labels; alias list
isb inspect NAME [--json]              one sandbox, by service or instance name
isb exec TARGET [-u USER] [-w DIR] [-e K=V]... [-l] [-t|-T] [-i|-n] [--timeout D] -- ARGV...
isb prune --label KEY --missing-path [-y] [--json]
```

- `create` fails if the instance exists, unless `--ensure`, which
  reconciles it the way `up` does. `-v`, `-p` and `--ready` use the
  [CLI shorthands](compose.md#cli-shorthands); relative bind paths resolve
  against the current directory. `--no-ready` skips readiness checks.
- `--egress` (repeatable) confines the sandbox's network to those hosts
  (`HOST[:PORT]`, port 443 by default, `*.example.com` for subdomains), or to
  nothing with `none`. `--secret NAME[=SECRET]@host1,host2` gives it the org
  secret as a placeholder in `$NAME`, swapped for the real value on the wire
  to those hosts only. Both are [`egress`](compose.md#egress) in a compose
  file; [Sandbox egress and secrets](../guides/egress.md) has the guarantees.
- `start` and `restart` wait for `running` only, not a file's `ready`
  checks.
- `exec` runs argv with no shell. `TARGET` is a compose service when a
  compose file defines one by that name, or the instance name of one of its
  services (`myproject-web`), else any instance. A service runs as its `user`
  in its `working_dir` unless `-u`/`-w` say otherwise; an instance that is
  not in the compose file in the current directory runs as the instance's
  default user (root) in its default directory. A terminal is allocated when
  stdin is one (`-t` forces it, `-T` never). Stdin is forwarded when it is a
  terminal, with `-T` (the pipe form, for a server that speaks on stdin and
  stdout), or with `-i`; otherwise the command sees EOF, so a script that
  calls `isb exec` keeps its own stdin, and `isb exec web -i -- cat < f`
  feeds a file. Piped input left unread is said on stderr (`stdin is not
  forwarded without -i`). `-n` gives the command an empty stdin even from a
  terminal, and keeps that line quiet;
  `-l` runs it through the user's login shell; `--timeout` kills it (no
  limit by default).
- `prune` deletes sandboxes whose `KEY` label is an absolute host path that
  no longer exists. It is a dry run unless `-y`, and never touches a
  sandbox without the label or whose path still exists.

## Compose files

```text
isb up [SERVICE...] [-d] [--no-log-prefix] [-t 10s] [--prune-devices] [--no-ready] [--json]
isb plan [SERVICE...] [--prune-devices] [--json] [--exit-code]
isb down [SERVICE...] [--volumes]
isb ps [SERVICE...] [--json]           status per service; without a compose file, running sandboxes
isb config [--services]                the resolved file (or only the service names)
isb logs SERVICE [-n 100]              a long-running (restart:) or OCI service's recent output
isb schema                             JSON Schema of the compose format
```

**`up` runs in the foreground**, like `docker compose up`: it creates or
reconciles the sandboxes, runs each service's `command`, streams its output
with a `<service> | ` prefix, and stops the sandboxes when the commands
exit, on Ctrl-C, or when the process that started isb goes away. `-d`
returns once they are up and leaves them running; `-t` bounds the clean
shutdown before a kill. See [Foreground `up`](compose.md#foreground-up).
A service with `restart` has its `command` supervised inside the guest, so
it outlives isb and host reboots; `up` then follows its output instead, and
`isb logs SERVICE` shows it later ([`restart`](compose.md#restart)).

`down --volumes` also deletes the file's non-external named volumes, unless
they are still in use; with a service list it is ignored.

## Volumes, ports and devices

```text
isb volume create NAME [--pool P] [-c KEY=VALUE]...   no-op if it exists
isb volume ls [--pool P] [--json]
isb volume inspect NAME [--pool P] [--json]       JSON either way
isb volume rm NAME [--pool P]                          refused while in use

isb port add NAME SPEC [--name DEV] [--search N]       prints the listen address in use
isb port get NAME DEV [KEY] [--json]                   one property (default listen); --json: every property
isb port rm NAME DEV...
isb port ls NAME [--json]

isb device ls NAME [--json]                            instance-local devices
isb device rm NAME DEV...                              (root cannot be removed)
```

These talk to incus directly. `port add` leaves a correct device alone;
`--search N` steps past up to N taken host ports. The org's named-volume
snapshots, backups and restores are below, under [Platform:
volumes](#volumes-snapshots-and-restores).

## The daemon

```text
isb serve [FLAGS]                       run the daemon
isb serve install [--listen ADDR] [--machine NAME] [--no-host-setup]
isb tui                                 the live dashboard
isb ingress [--json]                    routed domains, certificates, conflicts, tunnels
```

`serve install` writes and starts the systemd user unit, running `sudo isb
host setup` first on a host that has not had it (`--no-host-setup` skips
that); on macOS it writes a LaunchAgent that starts the `isb machine` at
login (`--machine` picks which).
`--listen` defaults to the env file's, else `127.0.0.1:8092`. Every flag of
`isb serve` is in [Configuration](configuration.md#daemon-flags). `isb tui`
is described in [isb tui](tui.md); without a daemon it shows sandboxes only.

## Stacks

```text
isb stack deploy [NAME] [-f FILE]... [-d] [--timeout 10m] [--project P [--env E]] [--no-reuse-secrets]   waits for the rollout unless -d
isb stack ls [--json]               every org you see (an ORG column), or --org ORG's
isb stack ps NAME [--json]
isb stack logs NAME SERVICE [--slot N] [-n|--tail 100] [--since 10m|TIME] [--failed]
isb stack exec NAME SERVICE [--replica|--slot N] [-u USER] [-w DIR] [-e K=V]... [-i] [--timeout 60s] -- CMD...
isb stack scale NAME SERVICE=N...
isb stack redeploy NAME SERVICE
isb stack rollback NAME [--to ID]   ID: a kept deployment (isb stack deployments)
isb stack deployments NAME [--json]
isb stack env NAME                  the stack's environment (.env text the daemon resolves ${VAR} with)
isb stack env-set NAME [FILE] [--deploy]   replace it from FILE or stdin
isb stack config NAME
isb stack rm NAME [--volumes]
```

`deploy` reads the file as `isb up` would (`.env`, `--env-file`, `${VAR}`),
including `file:` and `environment:` secrets, and exits 0 once every service
converged, 1 if one paused or is failing. The stack's name defaults to the
compose project name. A new stack belongs to `--project`/`--env` (the
project is made if missing; the environment defaults to `production`), or
to the project named like it; an existing stack keeps its project, and
naming another is refused. A stack with an environment on the daemon
(`isb stack env-set`) has its `${VAR}` resolved there, from one compose
file. A `file:`/`environment:` secret that gets no value reuses the one an
earlier deploy stored, with `warning: secret KEY: no value given; reusing
the value stored on DATE` on stderr (`deploy`, `rollback --to` and
`env-set --deploy` all print it); `--no-reuse-secrets` fails the deploy
instead. See [Stacks](../concepts/stacks.md) and
[Compose stacks in an environment](../concepts/apps.md#compose-stacks-in-an-environment).

## Orgs and the host

```text
isb org create NAME [--cpus N] [--memory SIZE] [--disk SIZE] [--instances N]
                    [--default-cpus N] [--default-memory SIZE] [--bind-root DIR]...
                    [--allow-egress DEST]... [--allow-domain SUFFIX]... [--allow-udp IP:PORT]...
                    [--ingress caddy|cloudflare-tunnel] [--cloudflare-account ID] [--cloudflare-zone ID]
                    [--server SERVER | --vm [--vm-cpus 2] [--vm-memory 4GiB] [--vm-disk 40GiB]]
isb org update NAME [--cpus N|none] [--memory SIZE|none] [--disk SIZE|none] [--instances N|none]
                    [--default-cpus N] [--default-memory SIZE]
                    [--allow-egress DEST]... [--allow-udp IP:PORT]... [--json]
isb org ls [--json]                                    alias list
isb org show NAME [--json]
isb org rm NAME [--force] [--delete-vm]
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run] [--public-ingress] [--sandbox-egress]
```

`org create` on an existing org sets the flags given. `org update` changes
an existing org through the daemon (the `org_update` tool): flags left out
keep their value, and `none` lifts a limit. `--disk SIZE` is refused while
an instance in the org has no root size, naming each (give its service
`raw_devices: {root: {size: ...}}` and redeploy it, or delete it); under the
limit, each new instance gets its own root size (its spec's, else 10GiB).
`org show` prints each limit with
what the org's instances are allocated against it
(`cpus       3 of 4 allocated, 1 free`), and `--json` has it as `allocation`.
`--allow-egress`, `--allow-domain` and `--allow-udp` replace the org's lists
(`none` clears them). `--server` and `--vm` place the org once, through the local
daemon (`--vm` takes a few minutes; rerun to retry). `org rm` refuses an
org with instances unless `--force`; `--delete-vm` also deletes a dedicated
VM. See [Orgs](../concepts/orgs.md), [Placement](../concepts/placement.md)
and [Host firewall](../operations/host-setup.md#host-firewall).

## Servers

```text
isb server add NAME --ssh USER@HOST --key FILE [--port 22] [--address A] [--agent-port 7443]
               [--allow-from CIDR]... [--isb-binary FILE | --isb-version V | --self-binary]
               [--public-ingress]
isb server ls [--json]                  alias list
isb server show NAME [--json]           JSON either way
isb server rm NAME                      refused while orgs are placed on it; alias remove
isb server rotate-cert NAME
isb server upgrade NAME|--all [--isb-version V | --isb-binary FILE]
```

Platform admins, through the local daemon (the control plane). See
[Servers and dedicated VMs](../guides/servers.md).

## Workspaces

```text
isb workspace create [--image IMAGE] [--name N] [--user dev] [--cpus N] [--memory SIZE]
                     [--root-size SIZE] [--home-size 20GiB] [-e KEY=VALUE]... [--secret NAME]...
                     [--token-role viewer|member|admin] [--home-bind DIR]
isb workspace show [NAME] [--json]      alias get
isb workspace ls [--json]               alias list
isb workspace start [NAME]
isb workspace stop|restart [NAME] [--yes]
isb workspace rebuild [NAME] [--image IMAGE] --yes
isb workspace update [NAME] [--image I] [--cpus N] [--memory M] [--root-size S]
                     [--home-size S] [--token-role R] [--yes]
isb workspace rm [NAME] [--keep-home] --yes          alias delete
isb workspace rotate-token [NAME]
isb workspace setup [NAME] [--run]
isb workspace image build [NAME] [--recipe FILE] [--base IMAGE] [--description T] [--timeout 30m] [--force]
isb workspace image ls|logs ID|rm NAME
isb workspace settings [--max-workspaces N] [--sandbox-expiry 24h] [--sandbox-idle 2h|none]
                       [--home-kind volume|host] [--home-pool POOL]
isb workspace sandboxes [--json]
isb workspace extend SANDBOX [--by 24h] [--idle-timeout 4h|none]
isb workspace ssh-config [--name N] [ssh-config flags]
isb workspace ssh [--name N] [--as EMAIL] [--url URL] [--token-file FILE]
```

`--yes` confirms what ends live sessions. `--home-bind` is for
superadmins; `--max-workspaces`, `--home-kind` and `--home-pool` for
platform admins. See [Workspaces](../concepts/workspaces.md).

## Secrets

```text
isb secret create NAME [FILE|-] [--driver local] [-l KEY=VALUE]...   value from FILE or stdin, never argv
isb secret set NAME [FILE|-]           a new version (creates it if missing)
isb secret get NAME                    the raw value, to stdout
isb secret ls [--json]                 metadata only; alias list
isb secret inspect NAME [--json]       metadata only
isb secret rm NAME...                  refused while a deployed stack uses one
isb secret encrypt [FILE|-] [-r RECIPIENT]...   armored age for a compose `age:` field
isb secret reencrypt [--all]           to the current recipients
isb secret refresh NAME                re-read from an external driver
```

See [Secrets](../guides/secrets.md).

## Projects and apps

```text
isb project create NAME [--env E]... [--description D]    environments default to production
isb project ls [--json]                                   alias list
isb project rm NAME                                       a project with no apps
isb project env-add PROJECT ENV
isb project env-rm PROJECT ENV

isb app create NAME --project P [--environment E]
               (--image REF | --git URL [--ref main] [--subdir D]
                [--token-secret S | --ssh-key-secret S]
                [--builder railpack|nixpacks|dockerfile|buildpacks] [--dockerfile PATH]
                [--build-arg K=V]...)
               [-e K=V]... [--env-from FILE] [--port N] [--replicas N] [-p SPEC]...
               [-v NAME:/path[:ro]]... [--domain HOST[/PATH]]... [--command CMD]
               [--cpus N] [--memory M] [--deploy]
isb app ls [--project P] [--json]
isb app show NAME [--json]              alias get; --json prints app_get's answer
isb app update NAME [-f PATCH|-] [--image REF] [--ref R] [--replicas N] [--port N]
               [--cpus N] [--memory M] [--deploy]
isb app rm NAME                         named volumes are kept
isb app deploy NAME [-d]                follows the deployment's log; exit 0 when done
isb app rollback NAME [ID] [-d]         a previous deployment's image and settings, no build
isb app deployments NAME [--json]
isb app deploy-log NAME [ID] [-f]      a deployment's build and rollout log
isb app logs NAME [--replica N] [-n 200] [--since 10m]   the replicas' output, by app name
isb app exec NAME [--replica N] [-u USER] [-w DIR] [-e K=V]... [-i] [--timeout D] -- ARGV...
isb app restart NAME [--wait]           rolling replace, same settings
isb app scale NAME N                    also saved as the app's replicas
isb app top NAME [--json]               per replica CPU and memory
isb app events NAME [--stack-wide] [--json]
isb app env NAME                        the environment as .env text
isb app env-set NAME [FILE|-] [--deploy]
isb app webhook NAME [--rotate]
isb app deploy-key NAME
isb app previews ls [NAME] [--json]
isb app previews show NAME PR [--json]
isb app previews logs NAME PR [ID] [-f]
isb app previews redeploy NAME PR [-d]
isb app previews rm NAME PR
```

`app update -f` takes a JSON or YAML merge patch (`-` for stdin); `--cpus`
and `--memory` set `resources` per replica, keeping the one not given. In
a patch or a tool call, `resources.cpus` and `resources.memory` may be numbers
(`{"cpus": 2}`) or strings (`"2"`, `"512m"`); a bare number of memory is bytes. See
[Deploy apps](../guides/deploy-apps.md) and
[Preview deployments](../guides/previews.md).

## Instances

What runs in an org, like `kubectl get pods`, `describe`, `exec` and `cp`
([isb for kubectl users](../guides/kubectl.md)):

```text
isb instance ls [--app A] [--stack S] [--service V] [--kind K] [--status S] [--json]   aliases list, ps
isb instance get NAME [--json]          alias describe
isb instance exec NAME [-u USER] [-w DIR] [-e K=V]... [-i] [--timeout D] -- ARGV...
isb instance restart NAME [--wait]      a replica is replaced, a sandbox restarts
isb cp INSTANCE:/path LOCAL             a small file out (at most 4 MiB)
isb cp LOCAL INSTANCE:/path             and in (at most 2 MiB)
```

`--kind` is `app`, `database`, `stack`, `tunnel`, `workspace`, `build` or
`sandbox`. `exec` runs argv with no shell, prints the command's output and exits
with its status (124 when `--timeout`, default 60s and at most 15m, killed it);
`-i` feeds this process's stdin (at most 1 MiB); without it, piped input is
left unread and said on stderr, which `-n` keeps quiet. These go through the daemon's
tools, so they work from anywhere `ISB_URL` and `ISB_TOKEN` reach it, as
members and up; `isb exec` is the one for sandboxes on this host.

## Templates

```text
isb template ls [WORDS...] [--tag T] [--catalog C] [--json]
isb template show REF [--json]          variables, apps, notes; a Dokploy or Coolify template's translation report
isb template deploy REF --project P [--env E] [--name N] [-s KEY=VALUE]... [--dry-run] [-d] [--json]
isb template instances [--json]
isb template rm NAME                    its apps (volumes kept) and secrets
isb template catalog ls [--json]
isb template catalog add NAME [--format native|dokploy|coolify] LOCATION   platform admins
isb template catalog rm NAME                                       platform admins
```

See [Templates](../guides/templates.md).

## Databases, backups and jobs

```text
isb db create NAME --project P [--environment E] --engine ENGINE[:TAG] [--database D] [--user U]
              [--publish [IP:]PORT] [--url SECRET[?QUERY]]... [--cpus N] [--memory M]
              [--no-deploy]      postgres, mysql, mariadb, mongodb, redis
isb db ls [--project P] [--json]
isb db show NAME [--show-password] [--json]
isb db rm NAME                          its volume and credentials are kept

isb backup dest create NAME --endpoint URL --bucket B [--region us-east-1] [--prefix P] [--path-style]
              (--access-key ID | --access-key-secret S --secret-key-secret S)
              [--create-bucket] [--no-test]      the secret key from $ISB_S3_SECRET_KEY or stdin
isb backup dest ls [--json] | rm NAME | test NAME
isb backup create NAME (--database DB | --volume V) --destination D --schedule CRON
              [--keep 7] [--compression gzip|zstd|none] [--timezone +HH:MM]
isb backup update NAME [--schedule CRON] [--keep N] [--destination D] [--enable|--disable]
isb backup ls [--json]
isb backup files NAME [--json]
isb backup run NAME [-d]
isb backup runs [NAME] [--restores] [--json]
isb backup logs [NAME] [RUN] [--restore] [-f]
isb backup restore [BACKUP] [--destination D --key K] [--key K]
              (--into DB [-y] | --new NAME [--project P] [--environment E]) [-d]
isb backup rm NAME                      its files stay in the bucket

isb job create NAME --schedule CRON (--app A | --stack S --service SVC) [--mode exec|run]
              [--timeout 10m] [--concurrency skip|allow] [--keep 20] [--timezone +HH:MM]
              [-u USER] [-e K=V]... [--disabled] -- COMMAND...
isb job ls [--json] | show NAME [--json] | rm NAME
isb job update NAME [--schedule CRON] [--timeout D] [--enable|--disable] [-- COMMAND...]
isb job run NAME [-d]
isb job runs NAME [--json]
isb job logs NAME [RUN] [-f]
```

`job create --disabled` keeps the job without running it on its schedule
until `job update NAME --enable`. `job ls --json` prints one object per
job: its settings (`name`, `schedule`, `target`, `command`, `enabled`, ...)
with `created_at`, `updated_at`, `next_run` and `last_run` beside them, as
`job_get` answers. `backup restore --into` asks before replacing the
database's data (`-y` skips the question). See [Databases](../guides/databases.md) and
[Scheduled jobs](../guides/jobs.md).

## Volumes: snapshots and restores

Through the daemon, for an org's named volumes:

```text
isb volume show NAME [--json]           snapshots, schedule, backups, staged restores (JSON either way)
isb volume snapshot create NAME [--as SNAP] [-d]
isb volume snapshot ls NAME [--json]
isb volume snapshot rm NAME SNAP
isb volume snapshot schedule NAME [--schedule CRON | --off] [--timezone TZ] [--keep 7]
                                  [--hook-timeout 5m] [--hook-required | --hook-optional]
isb volume snapshot runs NAME [--json]
isb volume snapshot logs NAME [RUN]
isb volume restore NAME (--snapshot S | --backup B [--key K] | --destination D --key K)
                   [--instance I] [-d]
isb volume restores [NAME] [--json]
isb volume discard NAME STAMP
```

See [Volumes](../guides/volumes.md).

## Builds and the registry

```text
isb build DIR --app APP [--tag latest] [--builder railpack|nixpacks|dockerfile] [--dockerfile PATH]
              [--target STAGE] [--arg K=V]... [--subdir DIR] [--untrusted] [--timeout 30m] [-d]
isb registry setup [--port 5480] [--renew] [--state-dir DIR]   on incus directly, not the daemon
isb registry ls [--json]
isb registry gc [--keep 10] [--dry-run]                        platform admins
```

`build` prints `registry:APP:TAG@sha256:...` for a compose `image:`; `-d`
prints the build id instead. See [Builds](../guides/builds.md).

## Notifications

```text
isb notify create NAME (--webhook URL_SECRET [--signing-secret S] | --slack URL_SECRET
               | --discord URL_SECRET | --telegram TOKEN_SECRET --chat-id ID
               | --smtp-host H [--smtp-port P] [--smtp-tls starttls|tls|none]
                 [--smtp-user U --smtp-password-secret S] --from ADDR --to ADDR...)
               [--events deploy.*,health.*] [--app-project P]... [--app A]... [--stack S]...
               [--disabled]
isb notify ls [--json] | show NAME [--json] | rm NAME
isb notify update NAME [--events ...] [--app-project P]... [--app A]... [--stack S]...
               [--enable|--disable]         any rule flag replaces the rules with one rule
isb notify test NAME                        exit 1 if it failed
isb notify deliveries NAME [-n 20] [--json]
isb notify settings [--allow-private-targets true|false]   platform admins
```

See [Notifications](../guides/notifications.md).

## Users, invitations and tokens

On the daemon's identity store, `<state>/isb.db`, as the daemon's user;
each takes `--state-dir DIR` (`ISB_SERVE_STATE_DIR`).

```text
isb user create EMAIL [--admin] [--name N]   password prompted twice, or stdin's first line
isb user ls [--json]
isb user passwd EMAIL                         set a password and end their sessions
isb invite ORG EMAIL [--role member]          viewer, member, admin or owner
isb token create NAME [--org ORG] [--expires 90d] [--user EMAIL] [--scope S]...
isb token create NAME --superadmin [--expires 30d]
isb token ls [--json]
isb token revoke ID|sa-ID...
isb superadmin ls [--json]
isb superadmin add --access EMAIL | --access-token CLIENT_ID | --tailnet LOGIN_OR_TAG
isb superadmin rm  --access EMAIL | --access-token CLIENT_ID | --tailnet LOGIN_OR_TAG
```

- The first user is always a platform admin and owner of the `default` org.
- `invite` prints the invitation token once, or its link when
  `ISB_PUBLIC_URL` is set.
- `token create` prints the token once. `--org` confines it to one org
  (required unless the user is a platform admin); `--user` defaults to the
  only platform admin; `--scope` (`read`, `deploy`, `admin`, `tool:GLOB`,
  repeatable) only narrows the user's role. `--superadmin` mints a token
  that belongs to nobody and has the unix socket's reach; it is the only way
  to make one. See [Users, roles and superadmins](../concepts/access.md).
- `superadmin add` makes one tailnet or Access identity a superadmin, `rm`
  removes one it made; the running daemon picks either up at the next
  request, with no restart. They are the only way to change these
  identities: no HTTP caller can. `superadmin ls` asks the running daemon
  (`superadmin_list`), so it shows the `--superadmin-tailnet` and
  `--superadmin-access` entries too, each with `SOURCE` (`flag` or `state`)
  and `EFFECTIVE` (`NO` with a reason when the daemon has no Access or no
  tailnet listen address for it); with `--state-dir`, or when the daemon
  does not answer, it shows `isb.db`'s alone. See [Superadmin identities in
  isb.db](../concepts/access.md#superadmin-identities-in-isbdb).

Passwords never come from argv.

## SSH

```text
isb key add FILE|- [--name N] [--user EMAIL] [--url URL] [--token-file FILE]
isb key ls [--user EMAIL] [--json] [--url URL] [--token-file FILE]
isb key rm ID... [--user EMAIL] [--url URL] [--token-file FILE]
isb ssh-config [ORG/INSTANCE...] [--user U] [--as EMAIL] [--known-hosts FILE] [--identity FILE]
               [--isb PATH] [-o FILE] [--url URL] [--token-file FILE]
isb ssh-proxy ORG/INSTANCE [--as EMAIL] [--url URL] [--token-file FILE]
```

With `--url` (or `ISB_URL`) and a token (`ISB_TOKEN`, or `--token-file`),
`key` acts on the token's own account and `ssh-proxy` reaches a remote
daemon. Without, `key` edits the identity store on the host for `--user`
(default: the only platform admin), and `ssh-proxy` uses the local
daemon's socket with the keys of `--as`. `ssh-config` prints a `Host` block
per instance (every running instance in `--org` when none is named), pins
host keys in `~/.config/isb/known_hosts`, and prints a `herdr machine add`
line per instance on stderr. See [SSH and herdr](../guides/ssh.md).

## Audit and history

On `<state>/audit.db`, as the daemon's user (`--state-dir`).

```text
isb audit ls [--org ORG | --platform] [--actor GLOB] [--action GLOB] [--target GLOB]
             [--outcome ok|error|CODE] [--since 24h] [--until 1h] [-n 50] [--json]
isb audit export [same filters]          every match as JSON lines, oldest first
isb audit verify                         both hash chains (audit, history); exit 1 if one breaks

isb history NAME                         NAME's timeline (instance, image, alias, volume, stack, app),
                                         oldest first, with inferred causes
isb history [--object NAME [--exact]] [--org ORG | --platform] [--source audit,controller,incus,marker]
            [--kind GLOB] [--actor GLOB] [--since 24h] [--until 1h] [-n 50] [--json] [--export]
```

See [The audit log](../operations/audit.md) and
[The history](../operations/history.md).

## Updating isb

```text
isb update [VERSION] [--check] [--force]
```

Replaces the running binary with the latest release (or `VERSION`, which may
be older, down to 1.1.1, the first signed release). The release's
`SHA256SUMS` must carry a valid signature by the isb release key
([Releases are signed](../getting-started/install.md#releases-are-signed)),
and the tarball for this platform must match it; it is then unpacked next
to the binary, run once with `--version`, and renamed over it, so a failed
update leaves the old binary in place.
`--check` only compares versions. A binary installed by cargo, npm or pip is
refused with that manager's upgrade command, since replacing it would leave
the manager's record wrong. A mise install is refused with the installer's
command instead, since mise installs are deprecated (mise does not check the
release signature); `--force` replaces it anyway, and also
reinstalls the same version. A running `isb serve` keeps the old binary
until it restarts ([Upgrading isb](../operations/upgrades.md)).

## macOS: the isb machine

```text
isb machine init [NAME] [--cpus 4] [--memory 4GiB] [--disk 10GiB] [--isb-binary PATH] [--timeout 20m]
isb machine start [NAME]                start and wait for incus and isb serve
isb machine stop [NAME]                 sandboxes and stacks in it stop too
isb machine rm [NAME]                   the VM, everything in it, and its LaunchAgent
isb machine status [NAME] [--json]
isb machine ssh [NAME] [-- ARGV...]
```

`NAME` defaults to `isb`. See [isb on macOS](../getting-started/macos.md).

## The SDK protocol

```text
isb rpc                                 line-delimited JSON on stdin/stdout
```

The protocol the Python and TypeScript SDKs speak; see
[rpc protocol](rpc.md).
