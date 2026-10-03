# isb CLI reference

```sh
isb --help            # every command
isb COMMAND --help    # one command's flags
```

```text
isb create NAME -i IMAGE [--vm] [--cpus N] [--cpuset-cpus SET] [-m MEM] [-v SRC:GUEST[:ro,owner=U]]
                         [-p [IP:]PUBLISHED:TARGET]
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

# compose (-f FILE, repeatable; default ./isb.yaml plus ./isb.override.yaml)
isb up [SERVICE...] [-d] [--no-log-prefix] [-t D] [--prune-devices] [--no-ready] [--json]
isb plan [SERVICE...] [--json] [--exit-code]
isb down [SERVICE...] [--volumes]
isb ps [SERVICE...] [--json]
isb exec SERVICE -- ARGV...
isb logs SERVICE [-n 100]                  output of a long-running (restart:) or OCI service
isb config

# stacks, on the isb serve daemon (docs/stacks.md, docs/serve.md)
isb serve [--listen 127.0.0.1:8092] [...]  run the daemon
isb serve install                          as a systemd user service (macOS: a LaunchAgent starting the machine)
isb stack deploy [NAME] [-d] [--timeout D]
isb stack ls | ps NAME | logs NAME SERVICE | config NAME
isb stack scale NAME SERVICE=N... | redeploy NAME SERVICE | rollback NAME
isb stack rm NAME [--volumes]

# orgs: incus projects with their own network (docs/orgs.md)
isb org create NAME [--cpus N] [--memory M] [--bind-root DIR]... [--allow-egress CIDR[:PORTS[/tcp|udp]]]...
isb org ls [--json] | show NAME [--json] | rm NAME [--force]
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run]   firewall for org bridges, service-name dir

# secrets, per org, on the isb serve daemon (docs/secrets.md); all take --org ORG
isb secret create NAME [FILE|-] [--driver D] [-l k=v]   value from FILE or stdin, never argv
isb secret set NAME [FILE|-]               a new version
isb secret get NAME                        the raw value, to stdout
isb secret ls [--json] | inspect NAME [--json]   metadata only
isb secret rm NAME...                      refused while a deployed stack uses it
isb secret encrypt [FILE|-] [-r RECIPIENT]...   armored age for a compose `age:` field
isb secret reencrypt [--all]               to the current recipients
isb secret refresh NAME                    re-read from an external driver
isb tui                                    live dashboard (docs/tui.md)

# apps, per org, on the isb serve daemon (docs/apps.md); all take --org ORG
isb project create NAME [--env E]... [--description D]   environments default to production
isb project ls [--json] | rm NAME | env-add PROJECT ENV | env-rm PROJECT ENV
isb app create NAME --project P [--environment E] (--image REF | --git URL [--ref R] [--subdir D]
               [--token-secret S | --ssh-key-secret S] [--builder railpack|nixpacks|dockerfile|buildpacks]
               [--dockerfile PATH] [--build-arg K=V]...) [-e K=V]... [--env-from F] [--port N]
               [--replicas N] [-p SPEC]... [-v NAME:/path]... [--domain HOST[/PATH]]...
               [--command CMD] [--cpus N] [--memory M] [--deploy]
isb app ls [--project P] [--json] | show NAME | rm NAME
isb app update NAME [-f PATCH|-] [--image REF] [--ref R] [--replicas N] [--port N] [--deploy]
isb app deploy NAME [-d]                   follows the deployment's log; exit 0 when done
isb app rollback NAME [ID] [-d]            a previous deployment's image and settings, no build
isb app deployments NAME [--json] | logs NAME [ID] [-f]
isb app env NAME | env-set NAME [FILE|-] [--deploy]   the environment as .env text
isb app webhook NAME [--rotate] | deploy-key NAME

# identity for isb serve, on <state>/isb.db directly (docs/auth.md)
isb user create EMAIL [--admin] [--name N]  password from the terminal, or stdin's first line
isb user ls [--json] | passwd EMAIL
isb invite ORG EMAIL [--role member]       prints the invitation token (or link), once
isb token create NAME [--org ORG] [--expires 90d] [--user EMAIL]   prints the token, once
isb token ls [--json] | revoke ID...

# macOS: the Lima VM that runs incus and isb serve (docs/macos.md); NAME defaults to isb
isb machine init [NAME] [--cpus 4] [--memory 4GiB] [--disk 10GiB] [--isb-binary PATH] [--timeout 20m]
isb machine start|stop|rm [NAME]
isb machine status [NAME] [--json]
isb machine ssh [NAME] [-- ARGV...]
```

**`up` runs in the foreground**, like `docker compose up`: it runs each
service's `command`, streams its output, and stops the sandboxes when the
commands exit, on Ctrl-C, or when the process that started isb goes away. `-d`
returns once they are up. See
[spec.md](spec.md#foreground-up).

**Long-running services.** A service with `restart` has its `command`
supervised inside the guest (a systemd unit, or incus for an OCI image), so it
survives isb and host reboots. A foreground `up` then follows its output rather
than running it, and `isb logs SERVICE` shows it later. See
[spec.md](spec.md#restart).

**Exit codes.** `isb exec` exits with the command's own status. A foreground
`isb up` exits with the first failing command's status, 128+N on signal N, or
129 when the process that started it went away. If isb itself
fails (the sandbox does not exist, incusd is unreachable) it exits 125. `isb plan
--exit-code` exits 2 when there are changes. Everything else exits 0 on success
and 1 on error.

**`prune`** is a dry run unless given `-y`, and never touches a sandbox without
the label, or one whose path still exists.

**Compose files** are found as `./isb.yaml` (or `isb.yml`), with
`./isb.override.yaml` merged over it if present, unless `-f FILE` is given;
several `-f` files merge in order. `.env` next to the first file supplies
`${VAR}`s unless `--env-file` is given. See
[spec.md](spec.md#files-and-validation).

**Global flags:** `--socket PATH` (default `$INCUS_SOCKET`, else
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`; on macOS,
`~/.isb/machine/isb/incus.sock`), `--project NAME`
(incus project), `-f FILE` (compose file, repeatable), `-P NAME` (compose
project name), `--env-file FILE`, `--create-timeout DURATION`, `-q`.
