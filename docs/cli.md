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
isb config
```

**`up` runs in the foreground**, like `docker compose up`: it runs each
service's `command`, streams its output, and stops the sandboxes when the
commands exit, on Ctrl-C, or when the process that started isb goes away. `-d`
returns once they are up. See
[spec.md](spec.md#foreground-up).

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
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`), `--project NAME`
(incus project), `-f FILE` (compose file, repeatable), `-P NAME` (compose
project name), `--env-file FILE`, `--create-timeout DURATION`, `-q`.
