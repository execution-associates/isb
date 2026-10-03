---
title: Install isb
description: Install incus and the isb binary on Linux or macOS, and the Python, TypeScript or Rust SDK if you want one.
order: 1
nav_title: Install
---

isb is a single static binary that drives [incus](https://linuxcontainers.org/incus/),
so installing it is two steps: have incus running, then put `isb` on your
`PATH`. Nothing else runs in the background until you ask for the daemon.

## Linux

### 1. incus

isb needs a Linux host with incus installed and initialised, and access to
its unix socket. Follow incus' own
[installation guide](https://linuxcontainers.org/incus/docs/main/installing/)
(on Ubuntu and Debian, [Zabbly's packages](https://github.com/zabbly/incus)
are the usual source), then:

```sh
sudo incus admin init --auto          # a storage pool and a bridge, if incus has none
sudo usermod -aG incus-admin "$USER"  # then log out and in again
incus info >/dev/null && echo ok      # isb needs this to work
```

Membership in `incus-admin` is **root-equivalent on that host**: anything
that can open the incus socket can do anything incus can. Treat every isb
call the same way, and keep the socket out of sandboxes.

Some features need more on the host:

| For | Needs |
|---|---|
| OCI images (`docker:`, `ghcr:`, `registry:` ...) | `skopeo` on the host, and incus 6.3 or later (the `instance_oci` API extension) |
| VMs (`type: vm`, untrusted builds, dedicated VMs) | KVM (`/dev/kvm`) |
| Org networks on a host with a default-deny firewall (ufw) | `sudo isb host setup` once ([host setup](../operations/host-setup.md#host-firewall)) |

### 2. The binary

```sh
mise use -g github:execution-associates/isb   # the static binary, nothing to compile
# or build it from source:
cargo install isb
```

Prebuilt static (musl) binaries for x86_64 and aarch64 Linux, and macOS
binaries for Apple silicon and Intel, are on the
[releases page](https://github.com/execution-associates/isb/releases), with
a `SHA256SUMS` file.

```console
$ isb --version
$ isb ls          # lists sandboxes; an error here means isb cannot reach incusd
```

isb finds incusd's socket at `--socket`, else `$INCUS_SOCKET`, else
`$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`.

## macOS

incus runs only on Linux, so on a Mac isb runs incus in a Linux VM that it
manages with [Lima](https://lima-vm.io):

```sh
brew install lima                            # Lima 2.0 or later
mise use -g github:execution-associates/isb  # or download the darwin binary from the releases page
isb machine init                             # first boot downloads Ubuntu and installs incus: a minute or two
```

Your home directory is shared with the VM at the same path and published
ports reach the Mac's localhost, so everything in these docs works as on
Linux. [isb on macOS](macos.md) has the details and the limits.

## The SDKs

The Python and TypeScript SDKs each bundle the binary; the Rust crate is the
same engine as a library.

```sh
pip install isb-sdk                  # imported as `isb`
bun add @execution-associates/isb    # or npm install
cargo add isb
```

See [isb from code](../guides/sdk.md).

## The agent skill

[`SKILL.md`](https://github.com/execution-associates/isb/blob/main/SKILL.md) at the repository root teaches a coding agent to
use isb correctly: the compose file, `isb up` versus `isb up -d`, the safety
rules, and the MCP tools. Put it in your agent's skills directory (for
Claude Code, `~/.claude/skills/isb/SKILL.md`).

## Upgrading

Replace the binary (`mise up`, a new release, `cargo install isb` again).
Sandboxes and stacks keep running across an upgrade. If you run the daemon
as a service, run `isb serve install` again afterwards: when the binary's
path contains its version (a mise install), the unit runs that exact path.
See [upgrading](../operations/upgrades.md).

## Next

[Your first sandbox](first-sandbox.md).
