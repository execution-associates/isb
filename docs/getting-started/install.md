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
its unix socket.

**On Ubuntu and Debian, use the Zabbly stable repository, not the
distribution's `incus` package.** Ubuntu 24.04's package is incus 6.0, which
is too old for OCI images (`docker:`, `ghcr:`, `registry:`): those need incus
6.3 or later (the `instance_oci` API extension), and the first app in these
docs is one.
The [Zabbly packages](https://github.com/zabbly/incus) install a current
incus over the distribution's:

```sh
sudo mkdir -p /etc/apt/keyrings
sudo curl -fsSL https://pkgs.zabbly.com/key.asc -o /etc/apt/keyrings/zabbly.asc
sudo tee /etc/apt/sources.list.d/zabbly-incus-stable.sources >/dev/null <<EOF
Enabled: yes
Types: deb
URIs: https://pkgs.zabbly.com/incus/stable
Suites: $(. /etc/os-release && echo "$VERSION_CODENAME")
Components: main
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/zabbly.asc
EOF
sudo apt-get update
sudo apt-get install -y incus skopeo
```

On other distributions follow incus' own
[installation guide](https://linuxcontainers.org/incus/docs/main/installing/)
and make sure you get 6.3 or later (`incus --version`). Then:

```sh
sudo incus admin init --auto          # a storage pool and a bridge, if incus has none
sudo usermod -aG incus-admin "$USER"  # then log out and in again
incus info >/dev/null && echo ok      # isb needs this to work
```

`isb serve` and `isb host setup` ask incus which API extensions it has and
print a warning at the start when `instance_oci` is missing, naming the
incus version and pointing back here. Sandboxes and system-image apps still
work on an older incus; only OCI images do not.

Membership in `incus-admin` is **root-equivalent on that host**: anything
that can open the incus socket can do anything incus can. Treat every isb
call the same way, and keep the socket out of sandboxes.

Some features need more on the host:

| For | Needs |
|---|---|
| OCI images (`docker:`, `ghcr:`, `registry:` ...) | `skopeo` on the host, and incus 6.3 or later (the `instance_oci` API extension; not Ubuntu 24.04's own 6.0 package) |
| VMs (`type: vm`, untrusted builds, dedicated VMs) | KVM (`/dev/kvm`) |
| Org networks on a host with a default-deny firewall (ufw) | `sudo isb host setup` once ([host setup](../operations/host-setup.md#host-firewall)) |

### 2. The binary

Install isb with the installer:

```sh
curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh
```

It downloads the release for your platform (static musl binaries for
x86_64 and aarch64 Linux; macOS for Apple silicon and Intel), checks the
release signature and the checksum, and installs `isb` to `~/.local/bin`
(`/usr/local/bin` as root; set `ISB_INSTALL_DIR` to choose). It installs
nothing that does not verify. It needs `curl`, `tar` and OpenSSL 3 or later
(on macOS, `brew install openssl@3`: the system's LibreSSL cannot check the
signature). To pin a version, pass it: `... | sh -s -- 1.2.0`. To read the
script before running it, download it first:

```sh
curl -fsSLO https://github.com/execution-associates/isb/releases/latest/download/install.sh
less install.sh && sh install.sh
```

From then on isb keeps itself current with `isb update`, which checks the
same signature. The installer is the only supported way to install the
binary; the alternative is building it from source with `cargo install isb`.

**Do not install isb with mise.** mise installs of isb are deprecated: mise
downloads the release without checking its signature, which is the check
that catches a tampered release. If you have one, replace it:

```sh
curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh
mise unuse -g github:execution-associates/isb
isb serve install    # if you run the daemon as a service: points the unit at ~/.local/bin/isb
```

#### Releases are signed

Each release's `SHA256SUMS` is signed with the isb release key (Ed25519;
the signature is `SHA256SUMS.sig`, 64 raw bytes), and isb has the public key
built in: the installer, `isb update`, `isb machine init` and `isb server`
upgrades install nothing whose signature does not verify, so a release changed after it was
built is refused even when its checksums were changed to match. Releases
before 1.1.1 are unsigned and cannot be installed that way. The public key
is

```text
4f08d05a2ffaf58f40d4d0e658a9934e5246de1b472ccd2143af6c928adfd51a
```

To check a download by hand, use the GitHub build attestation each tarball
also carries, which ties it to the workflow run that built it:

```sh
gh attestation verify isb-v1.1.1-x86_64-unknown-linux-musl.tar.gz -R execution-associates/isb
```

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
brew install lima openssl@3   # Lima 2.0 or later; OpenSSL 3 for the installer's signature check
curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh
isb machine init              # first boot downloads Ubuntu and installs incus: a minute or two
```

Your home directory is shared with the VM at the same path and published
ports reach the Mac's localhost, so everything in these docs works as on
Linux. [isb on macOS](macos.md) has the details and the limits.

## The SDKs

The Python and TypeScript SDKs each bundle the release's signed binary,
checked against the release signature when the package is built; pip and npm
check only their registry's hashes when you install. Set
`ISB_BIN=$(command -v isb)` to have an SDK use the isb you installed instead.
The Rust crate is the same engine as a library, built from source.

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

`isb update` replaces the binary with the latest release (`cargo install
isb` again for a source build). Sandboxes and stacks keep running across an
upgrade. If you run the daemon as a service, restart it afterwards
(`systemctl --user restart isb`). See [upgrading](../operations/upgrades.md).

## Next

[Your first sandbox](first-sandbox.md).
