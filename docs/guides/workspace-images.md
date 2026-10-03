---
title: Workspace images and recipes
description: Build the images workspaces start from with a recipe script, use isb's default image (Claude Code, Codex, herdr, mise), and tweak each workspace on its first boot.
order: 16
---

A workspace is made from an incus image. Any image works (`dev-base`,
`images:ubuntu/24.04`, the org's own `registry:APP:TAG`), but the useful
ones have the workspace user, a toolchain and the agents' CLIs baked in, so
a new workspace (or a rebuilt one) is ready the moment it starts. isb builds
such images from **recipes**: shell scripts it runs as root in a throwaway
container, then publishes as local images. Small per-org changes go in a
workspace's **first-boot script** instead, so they need no image of their
own.

```sh
isb workspace image build                         # isb's default image, as isb-workspace
isb workspace image build team --recipe team.sh   # your own recipe
isb workspace image ls
isb workspace image rm team
isb --org acme workspace create --setup first-boot.sh
```

## The default image

isb ships one recipe of its own. `isb workspace image build` (or **Build
the default image** in the workspace create form) publishes it as
**`isb-workspace`**, which new workspaces then get by default: without
`--image`, a workspace is `isb-workspace` when the host has it, else
`dev-base`, else `images:ubuntu/24.04`. On titan it took 83 seconds and is
a 632 MiB image.

| What | How it is installed |
|---|---|
| Ubuntu 24.04 | the base, `images:ubuntu/24.04` |
| `dev`, uid 1000, passwordless sudo | the image's own uid-1000 user renamed, or a new one |
| `openssh-server`, git, build-essential, curl, jq, ripgrep, tmux and the usual tools | apt; sshd is installed but not running (isb's SSH starts `sshd -i` per connection), and its host keys are removed so each workspace makes its own |
| mise, with node (LTS), bun and uv | mise's installer into `/usr/local/bin/mise`; tools with `mise install --system` under `/usr/local/share/mise`, listed in `/etc/mise/config.toml`, with shims on every login shell's path |
| Claude Code | the native build from `downloads.claude.ai`, checked against its release manifest's SHA-256, as `/usr/local/bin/claude`; `DISABLE_AUTOUPDATER=1`, since a rebuilt image is how it updates |
| Codex | the `codex-<arch>-unknown-linux-musl` asset of the latest `openai/codex` GitHub release, checked against the release's SHA-256 digest, as `/usr/local/bin/codex` |
| herdr | herdr's own installer (`https://herdr.dev/install.sh`, which checks its download) with `HERDR_INSTALL_DIR=/usr/local/bin` |

Everything lives outside `/home`, because a workspace's home (a volume or a
host folder) is mounted over `/home/dev` and would hide anything the image
put there. What `dev` installs later with `mise use -g`, `npm -g` or
`claude install` lands in the home and survives rebuilds; what the image
provides is replaced on each rebuild. The recipe bakes in no credentials:
agents sign in from inside the workspace, or read the org's secrets
(`--secret NAME`).

Claude Code is Anthropic's under its own terms and is downloaded when the
image is built on your host, never redistributed by isb; Codex and herdr are
Apache-2.0. The recipe is
[`crates/isb-apps/src/build/workspace-image.sh`](https://github.com/execution-associates/isb/blob/main/crates/isb-apps/src/build/workspace-image.sh).

Building again with the same recipe and base is a no-op (`--force` rebuilds,
to pick up newer releases); a newer isb with a changed default recipe builds
a new image under the same name, and `isb workspace image ls` says when
`isb-workspace` is from an older one. Existing workspaces get the new image
when they are rebuilt.

## Your own recipes

```sh
isb workspace image build NAME --recipe recipe.sh [--base images:ubuntu/24.04]
                               [--description TEXT] [--timeout 30m] [--force] [--no-follow]
isb workspace image ls [--json]
isb workspace image logs ID
isb workspace image rm NAME
```

A recipe is a shell script. It runs as root, with `DEBIAN_FRONTEND=noninteractive`
and a plain `PATH`, through its `#!` line when it has one and `/bin/sh`
otherwise. A non-zero exit fails the build. Start from the default recipe,
or from something as small as:

```sh
#!/bin/sh
set -eu
apt-get update -q
apt-get install -yq --no-install-recommends git postgresql-client
id dev >/dev/null 2>&1 || useradd -m -u 1000 -U -s /bin/bash dev
```

What a build does, in order:

1. Checks the name (a lower-case alias) and refuses one that names an image
   isb did not build: `dev-base`, say, can never be replaced or removed
   through isb.
2. Launches a container from the base in the **`isb-system` project**,
   isb's own, never an org's, on the host's default bridge (`incusbr0`),
   with 4 CPUs and 4 GiB.
3. Pushes the recipe in and runs it, streaming every line to the build's
   log (the CLI follows it; `workspace_image_logs` serves it), for at most
   `--timeout` (30 minutes, at most 2 hours).
4. Stops the container cleanly and publishes it as a local image with the
   asked alias and description, then deletes the container.

However it ends, the container is deleted, and a failed build publishes
nothing: the old image, if there was one, is untouched. A successful
rebuild moves the alias to the new image and deletes the old one when
nothing else names it. One build per name runs at a time.

The images isb builds carry `isb.workspace-image=1` among their properties,
with `isb.recipe-sha256`, `isb.base`, `isb.built-by` and `isb.built-at`.
That is what `ls` lists and what `rm` insists on. Images are the host's,
shared by every org on it (org projects use the host's images), so the image
tools are for platform admins and superadmins; every org's create form lists
the host's images with their descriptions. An org placed on another server
([Servers](servers.md)) uses that server's images.

| Tool | Does |
|---|---|
| `workspace_image_build` | `name` (default `isb-workspace`), `recipe` (default isb's), `base`, `description`, `timeout`, `force`; returns an `id` at once |
| `workspace_image_logs` | `id`, `since`, `wait`: the state (`running`, `succeeded` with `image`, `failed` with `error`) and log lines |
| `workspace_image_list` | the images isb built, recent builds, and whether the default image exists and is current |
| `workspace_image_remove` | `name`: the alias, and the image when nothing else names it; only isb's |

Each build is in the history as `workspace_image.build`.

## First-boot scripts

A workspace can carry a **setup script**: a few lines for one org that do
not deserve an image (a package, a clone, a config file).

```sh
isb --org acme workspace create --setup first-boot.sh
isb --org acme workspace update --setup first-boot.sh      # replace it
isb --org acme workspace update --no-setup                 # remove it
isb --org acme workspace setup                             # its state
isb --org acme workspace setup --run                       # run it again
```

- It runs **once, as root**, on the first start after the workspace is
  created and after each rebuild, on a thread of its own, so the create or
  rebuild returns at once. In `/root`, with `$ISB_ORG`, `$ISB_WORKSPACE`,
  `$ISB_USER` and `$ISB_HOME` set, for at most 30 minutes. Like a recipe, a
  `#!` line picks its interpreter.
- Its state is on the workspace (`setup_state`): `pending`, `running`,
  `succeeded` or `failed`, with the exit code, when, and how many runs. A
  workspace stopped before it ran runs it on its next start; a daemon
  restarted while it ran marks it failed rather than running it twice.
- Its outcome and the last 200 lines of its output are in the workspace's
  history (`workspace.setup`, an error when it failed); `--run`
  (`workspace_setup_run`, org admins) runs it again, now or on the next
  start.
- Changing it with `workspace_update` stores it for the next rebuild or
  `--run`; it does not run on its own.
- The script is part of the workspace's definition, which members can read:
  never put a secret in it. Deliver secrets with `--secret NAME` and read
  `/run/isb/secrets/NAME` from the script.

The web UI's create form has the field under the image picker.
