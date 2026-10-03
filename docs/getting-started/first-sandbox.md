---
title: Your first sandbox
description: Describe a sandbox in isb.yaml, bring it up, run commands in it, change it, and delete it.
order: 3
nav_title: First sandbox
---

A sandbox is a whole Linux machine (an incus system container, or a VM) with
only the directories you give it. You describe it in `isb.yaml`, and `isb up`
makes incus match: it creates what is missing and changes only what differs,
every time you run it. This page takes about five minutes.

## Write the file

In an empty directory:

```sh
mkdir -p site && echo '<h1>hello from a sandbox</h1>' > site/index.html
```

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

If you know docker compose you know the format: the same keys and syntax,
plus a few incus-only ones (`type: vm`, `idmap`, `ready`). The places it
differs are on purpose; ports, for one, default to `127.0.0.1`, not every
interface. The [isb.yaml reference](../reference/compose.md) lists every
field.

## Bring it up

```console
$ isb up
web | Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/) ...
```

The first run downloads the image, so it takes a little longer; after that a
container is usable in a few seconds. In another terminal:

```console
$ curl -s localhost:8000
<h1>hello from a sandbox</h1>
```

Like `docker compose up`, `isb up` stays in the foreground: it waits for the
`ready` checks, runs each service's `command`, streams its output with a
`<service> | ` prefix, and **stops** the sandboxes when the commands exit, on
Ctrl-C, or when whatever started isb goes away (a closed terminal, an agent's
background task ending), even without a signal. Stopped is not deleted: the
next `isb up` starts the same machine again with its state.

## Run commands in it

For scripts, and anything that should carry on after `up`, use `-d`: create
or reconcile, wait until ready, return.

```console
$ isb up -d
$ isb exec web -- ls -la                 # as the service's user, in its working_dir
$ isb exec web -- sh -c 'echo $HOME && whoami'
$ isb ps
```

`isb exec` takes argv after `--`, never a shell string: pipes, `&&` and globs
need `sh -c`. It exits with the command's own status, and with 125 when isb
itself failed (no such sandbox, incusd unreachable).

## Change it

Edit the file (say `cpus: 4`), then look before you leap:

```console
$ isb plan                 # what `up` would change
$ isb up -d                # change only that
```

`isb up` never touches a mount that is already right, so a dev server's file
watching keeps working through every `up`. Some things are fixed when the
sandbox is created (the image, the type, the storage pool); `plan` says so,
and `isb down` then `isb up` recreates it.

## Variables

`${VAR}` in the file is filled in from the environment when it is loaded, as
docker compose does it:

```console
$ WORKTREE=$PWD isb up
$ isb --env-file dev.env up       # KEY=VALUE lines; the environment wins
$ isb up                          # reads .env next to isb.yaml, if there is one
$ isb config                      # the file with every variable filled in
```

`${VAR:-default}` supplies a default, `${VAR:?message}` fails with a message,
and a plain `${VAR}` that is unset is an error rather than an empty string.

## Delete it

```console
$ isb down                 # delete the sandboxes; the host is untouched
```

`isb down --volumes` also deletes the file's named volumes.

## Without a file

For a one-off sandbox, flags do the same job:

```console
$ isb create task1 -i images:ubuntu/24.04 -v ./repo:/work -l owner=me --ready default_route
$ isb exec task1 -w /work -- make test
$ isb ls --label owner=me
$ isb rm -f task1
```

## A VM instead

Add `type: vm` and use a VM image (`images:ubuntu/24.04/cloud`) when you want
a separate kernel between the code and your machine: same file, same
commands. A VM takes tens of seconds to boot and waits for the incus agent
before `exec` works. See [containers vs virtual machines](../reference/compose.md#containers-vs-virtual-machines).

## Next

- [A dev environment per worktree](../guides/dev-environments.md): the
  patterns for real projects and for agents.
- [isb from code](../guides/sdk.md): the same thing from Python, TypeScript
  or Rust.
- [Your first org and app](first-app.md): keep things running with the
  daemon.
