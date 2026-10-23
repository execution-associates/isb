---
title: Run any CLI in a sandbox (isbx)
nav_title: isbx
description: Run npm, pip, uv, cargo, curl-pipe-bash installers and vendor CLIs in a sandbox instead of on the host, as easily as running them natively, with the project mounted at its own path and a closed network.
order: 17.5
---

A package install, a postinstall script or a vendor's CLI runs with
whatever its shell has. On a workstation or a server that means the SSH
keys, the 1Password session, tunnel credentials, the rest of `$HOME` and,
for a member of `incus-admin`, the incus socket, which is root. `isbx` runs
the command in a sandbox instead, and keeps it feeling local: the project
is mounted at its own path, so paths in arguments and output mean the same
thing; the terminal, pipes, exit codes and Ctrl-C behave as they would on
the host; and the network is closed except for what the command needs.

```sh
isbx npm install            # in the project you are in
isbx npx create-vite@latest app
cat data.json | isbx jq .   # pipes work
isbx --net public curl -fsSL https://example.com
isbx --vm bash ./unknown-script.sh   # its own kernel

isb x install sume --from 'sh:curl -fsSL https://sume.com/install.sh | bash' \
  --egress '*.sume.com' --egress @github --mount ~/sume
sume generate …             # a plain host command, run in sume's sandbox
```

`isbx` is the `isb` binary under another name; `isbx ARGS` is `isb x run
ARGS`. Make the name once:

```sh
ln -s "$(command -v isb)" ~/.local/bin/isbx
```

It talks to incus on its local socket, as `isb create` does, and needs
`isb serve` running on the same host for the network: the proxy that
enforces each sandbox's list is the daemon's
([Sandbox egress and secrets](egress.md)).

## One-off commands

`isbx CMD ARGS…` runs `CMD` in the **project sandbox** of the directory you
are in:

- **The project** is the nearest directory up from here that holds `.git`
  (a repository, or a worktree's `.git` file), else this directory.
  `--root DIR` names another. It is found by looking at the filesystem;
  git is never run on the host.
- **It is mounted at its own path.** `/home/you/app` is `/home/you/app`
  inside too, and the command runs in your current directory. Nothing else
  of the host is there: not `$HOME`, not `~/.ssh`, not the incus socket.
- **`.git` is read-only.** Nothing inside can write a hook, or set
  `core.hooksPath` in `.git/config`, that your own `git commit` would run.
  In a worktree, `.git` is a file pointing outside the mount, so git inside
  sees no repository at all. `--git-rw` mounts it writable.
- **It runs as `dev`, uid 1000,** in a `dev-base` sandbox (`--image`
  for another); files it writes are yours on the host (`idmap: auto`).
  Toolchains come from mise on dev-base, and the project's `mise.toml` is
  trusted inside the sandbox, never on the host.
- **Only display variables pass from your environment:** `TERM`,
  `COLORTERM`, `LANG`, `LC_*`, `TZ`, `NO_COLOR`, `FORCE_COLOR`,
  `CLICOLOR*`, `CI`, `COLUMNS`, `LINES`. `-e KEY` passes another from the
  host (its value never appears in a process list), `-e KEY=VALUE` sets one.
- **A terminal when you have one** (stdin and stdout both a terminal),
  separate stdout and stderr when you do not, stdin always forwarded,
  Ctrl-C and other signals passed on, and the command's own exit code.
  isbx's own failures exit 125 with a line starting `isbx:`.

isbx refuses to mount `/`, your home directory or any directory above it,
and anything that is, holds or sits inside a credential or host directory
(`~/.ssh`, `~/.config`, `~/.local`, `~/.gnupg`, `~/.claude`, `~/.aws`,
`~/.kube`, `~/.docker`, `~/.netrc`, `/etc`, `/run`, `/var/lib/incus` and the
like), judged after following symlinks. Run it from a project directory.

The project sandbox is **kept**: the first command in a project creates it
(tens of seconds, once), and later ones start it if stopped (about a second)
and exec in it (a few hundred milliseconds). `--fresh` makes a sandbox for
one command and deletes it afterwards, at the full creation cost.

## The network

Every sandbox isbx makes has a closed network: its own bridge, an allow
list, and `isb serve`'s proxy deciding every connection. What a project
sandbox may reach **grows with use**:

- It starts closed. DNS answers, so commands do not fail strangely, but the
  proxy refuses every connection.
- A package manager's first command adds what it needs, once, and the
  project keeps it: `npm`, `npx`, `pnpm`, `yarn`, `bun` and `node` add
  `@npm`; `pip`, `uv`, `uvx`, `pipx`, `python`, `poetry` add `@pypi`;
  `cargo` adds `@crates`; `go` adds `@go`; each also adds `@github` (release
  downloads that install scripts fetch) and `@mise` (runtimes). The
  presets are listed in [Sandbox egress and secrets](egress.md#the-rules).
  Adding takes about ten seconds.
- `--egress HOST[:PORT]` adds a host (`*.example.com`, a preset), the same
  way.
- Nothing is taken off a project's list in place: a command still running
  would lose the hosts it relies on. `isb x rm x-run-…` deletes the sandbox,
  and the next command starts a closed one.

When a command fails, isbx names the hosts the proxy refused it and how to
allow them:

```text
isbx: the sandbox was refused pypi.org; to allow it: rerun with --egress for each: pypi.org
```

Two other networks are sandboxes of their own (a sandbox cannot move
between them in place):

- `--net none`: no network at all, not even DNS.
- `--net public`: any public host on ports 443 and 80. The proxy still
  refuses private, loopback and link-local addresses, so this is the
  internet without the LAN, the tailnet or the host's own services.

A secret the command should use but never read goes on the wire only:
`--secret NAME[=SECRET]@host1,host2` gives it `$NAME` holding a
placeholder, which the proxy replaces with the value of isb's secret
`SECRET` (by default `NAME`, in the default org's store) in requests to
those hosts ([Secrets that never enter the guest](egress.md#secrets-that-never-enter-the-guest)).

## Installed tools

A CLI you use often gets a sandbox of its own, and shims on the host that
make its programs plain commands:

```sh
isb x install ruff --from pypi:ruff
isb x install cowsay --from npm:cowsay
isb x install gh-dash --from go:github.com/dlvhdr/gh-dash/v4
isb x install tool --from script:https://get.example.com/install.sh --sha256 9f86d0…
isb x install sume --from 'sh:curl -fsSL https://sume.com/install.sh | bash' \
  --egress '*.sume.com' --egress sume.com --secret SUME_API_KEY@api.sume.com --mount ~/sume
```

The recipe is one of:

| `--from` | Installs with | Network while installing |
|---|---|---|
| `npm:PKG[@VERSION]` | `npm install -g` | `@npm @github @mise` |
| `pypi:PKG` | `uv tool install` | `@pypi @github` |
| `cargo:CRATE` | `cargo install` | `@crates @github` |
| `go:MODULE[@VERSION]` | `go install` (`@latest` by default) | `@go @github` |
| `script:URL` | downloads the https installer, checks `--sha256` if given (and prints the hash it ran), runs it with bash | the URL's host |
| `sh:LINE` | any shell line, such as a vendor's `curl … \| bash` | only `--egress` |

plus every `--egress`. An installer whose hosts you do not know can run with
`--learn`: it installs with any public host open, then the tool keeps only
the hosts the install connected to.

isbx installs as `dev` (with passwordless sudo, so installers that write to
`/usr/local` work), then **finds the programs the install added** by
comparing the usual places before and after (`~/.local/bin`, `~/.cargo/bin`,
`~/go/bin`, `~/.bun/bin`, `/usr/local/bin`, any `~/.NAME/bin`, mise's
installs). `--bin NAME=/path/in/guest` (or `--bin /path`, or `--bin NAME`
to look it up on the guest's PATH) names them instead. Each becomes a shim:

```sh
$ cat ~/.local/share/isb/x/bin/sume
#!/bin/sh
# isbx shim: `sume` runs in the sandbox of isbx tool sume (`isb x rm sume` removes it)
exec isb x tool sume sume "$@"
```

**The shim directory must come first on PATH,** before version managers'
directories, or a host `npm` wins over a shim named `npm`:

```sh
export PATH="$HOME/.local/share/isb/x/bin:$PATH"   # in ~/.bashrc or ~/.zshrc
```

`isb x doctor` checks it. `isbx sume …` also finds an installed tool's
program.

What a tool's run mounts is `--mount`:

- `project` (the default): the project you run it in, as for one-off
  commands, mounted when the run starts. A run in another project replaces
  it (about three seconds), and a mount no running command uses is taken
  away, so a tool used in one project cannot write into another one used
  before.
- a host directory: always that one, at its own path (sume's output
  folder, say). The run's working directory is yours when you are inside
  it, else that directory.
- `none`: nothing; it runs in `/home/dev`.

The tool's own state (its config, a login it keeps, caches) lives in its
sandbox's home and never on the host. `isb x tool NAME CMD…` runs any
command there: `isb x tool sume bash` for a shell, `isb x tool sume sume
login` for a login flow.

### Secrets for tools

Two ways, chosen per secret:

- `--secret NAME[=SECRET]@hosts` (preferred): the tool sees a placeholder
  in `$NAME` and the proxy puts the real value on the wire to those hosts
  only. The value lives in isb's store for the default org; make it a
  1Password reference there and 1Password stays the one place it is kept
  ([1Password](secrets.md#onepassword): the default org needs its
  `onepassword-token` once). This needs a tool that trusts the sandbox's CA,
  which isb installs and points `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE` and
  `NODE_EXTRA_CA_CERTS` at; a tool with roots compiled in, or one that pins
  certificates, refuses it.
- `--secret-env VAR=op://vault/item/field`: read with `op read` on the
  host for each run and passed in that run's environment (the incus API
  request, never a process's arguments). It works with any tool, but the
  value is readable inside the sandbox while the command runs.

### Keeping tools current

- `isb x update NAME` (or `--all`) reinstalls from the recipe into a new
  sandbox generation (`x-NAME-2`, `x-NAME-3`, …) and switches only once the
  install worked and its programs exist; a failed install (a dead download,
  a missing release) leaves the working one in use. `--keep-failed` keeps
  the failed sandbox to look inside.
- `isb x allow NAME HOST…` and `isb x deny NAME HOST…` change a tool's list
  at once. The definition is `~/.config/isb/x/tools/NAME.yaml`: edit it and
  `isb x update NAME`.
- `isb x rm NAME` removes the tool's sandboxes, shims and definition.

## Shims for host commands

`isb x shim npm npx pnpm` makes the plain host commands run as one-offs in
the project sandbox (`exec isb x run -- npm "$@"`), so a habit, a script or
an agent typing `npm install` gets the sandbox without knowing. The real
program stays where it was; call it by its full path when you need it.
`isb x shim` lists the shims, `isb x shim --remove npm` removes one. A
shim never replaces a file isbx did not write, and `isb`, `isbx`, `sh`,
`bash`, `env` and `sudo` cannot be shims.

## Containers and VMs

A sandbox is a container unless `--vm` says otherwise:

- **A container** has its own users, filesystem and network; root inside is
  an ordinary user outside. It keeps the host's files, credentials, incus
  socket and networks out of reach. It shares the host's kernel, so a
  kernel exploit escapes.
- **A VM** (`isbx --vm …`, `isb x install --vm …`) has its own kernel. Use it
  for code nobody has read: an installer from a vendor you do not know, a
  package that looks wrong. It starts from `images:ubuntu/24.04/cloud`
  (dev-base is a container image), whose `ubuntu` user is uid 1000 like
  `dev` but has none of dev-base's toolchains: install what the command
  needs, or pass `--image` a VM image of your own. It boots in tens of
  seconds (once; it is kept like any other), host edits do not fire file
  watchers inside, and only uid 1000 can write the mounted project. A VM
  tool mounts `none` or a fixed directory.

What isbx does not stop: **the project is writable by design**, so code
inside can change files the host later runs or trusts (`package.json`
scripts, `.envrc`, `mise.toml` tasks, `.vscode/tasks.json`, a symlink you
later follow). Review changes to those as you would a pull request.

## Caches

Container sandboxes share one cache volume, `x-cache-ct`, at
`/var/cache/isbx`, and point npm, pip, uv, go and bun at it
(`npm_config_cache`, `PIP_CACHE_DIR`, `UV_CACHE_DIR`, `GOMODCACHE`,
`BUN_INSTALL_CACHE_DIR`), so the second project's install is fast. A VM,
which is for code not trusted to share, gets a cache of its own.
`--no-cache` uses none.

## Finding and cleaning up

```sh
isb x ls                       # project sandboxes and tools, status, last use, mounts
isb x stop                     # stop them all (the next command starts one in ~1s)
isb x gc --idle 30m            # stop those unused for 30 minutes
isb x gc --missing -y          # delete those whose project directory is gone
isb x rm x-run-1a2b3c4d5e      # delete one (its list starts closed again)
isb x doctor                   # isb serve, incus, the shims' place on PATH, every tool
```

`isb serve` stops an isbx sandbox that no command has used for 30 minutes
(an `isbx.idle` label, such as `user.isbx.idle=2h` in incus, sets another
time), and never one a command is running in; it never deletes one. The
next command starts it again in about a second.

Everything isbx makes is named `x-…` and labelled `isbx.kind` (`run` or
`tool`), with `isbx.root` (the project) or `isbx.tool` (the tool), so
`isb ls -l isbx.kind` finds it too.
