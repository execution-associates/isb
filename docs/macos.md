# isb on macOS

incus runs only on Linux, so on a Mac isb drives incus inside a Linux VM that
it manages, the way `podman machine` does: `isb machine`. The VM is a
[Lima](https://lima-vm.io) instance running Ubuntu 24.04 with incus from
[Zabbly](https://github.com/zabbly/incus)'s stable channel. Your home
directory is shared with it at the same path, and its sockets and loopback
ports are forwarded to the Mac, so `isb up`, `isb exec`, `isb stack` and
`isb tui` work on the Mac as they do on a Linux host.

## Install

```sh
brew install lima                            # Lima 2.0 or later
mise use -g github:execution-associates/isb  # or download the darwin binary from the releases page
isb machine init                             # first boot downloads Ubuntu and installs incus: a minute or two
```

`isb machine init [NAME] [--cpus 4] [--memory 4GiB] [--disk 10GiB]` creates
the machine `isb` unless given a name. The disk is the most it may grow to;
it starts at about 3 GiB. Then:

```sh
isb machine status       # state, resources, and whether incus and isb serve answer
isb up -d                # any compose file under $HOME
isb machine ssh          # a shell in the VM; `isb machine ssh -- incus list` runs one command
isb machine stop         # stops the VM, and with it every sandbox and stack
isb machine start        # starts it again and waits for incus and isb serve
isb machine rm           # deletes the VM and everything in it
```

The machine also needs a Linux isb for its daemon. `init` downloads the
release asset matching your isb's version from GitHub
(`isb-vX.Y.Z-<arch>-unknown-linux-musl.tar.gz`, checked against
`SHA256SUMS`); a development build passes its own with
`--isb-binary PATH` (an aarch64 or x86_64 `*-unknown-linux-musl` build, the
same architecture as the Mac).

## How it fits together

| On the Mac | In the VM |
|---|---|
| `~/.isb/machine/isb/incus.sock` | `/var/lib/incus/unix.socket` |
| `~/.isb/machine/isb/serve.sock` | `/run/isb/serve.sock` (`isb serve`) |
| `127.0.0.1:8092` | `127.0.0.1:8092` (`isb serve`'s HTTP listener) |
| `127.0.0.1:PORT` (1024 and above) | any loopback listener: published ports, the stack balancer |
| `$HOME` (`/Users/you`) | the same path, writable |

isb finds the machine without configuration: on macOS the incus socket
defaults to `~/.isb/machine/isb/incus.sock` and the `isb serve` socket to
`~/.isb/machine/isb/serve.sock`, after `$INCUS_SOCKET`/`$INCUS_DIR` and
`$ISB_SERVE_SOCKET`. A machine with another name prints the `export` line that
points isb at it (`isb machine status NAME`).

Everything the machine needs lives in `~/.isb/machine/NAME/`: the generated
Lima definition (`lima.yaml`), the guest's isb binary, the forwarded sockets,
and the LaunchAgent's log. Lima keeps the VM itself in `~/.lima/NAME`.

### Paths

A bind mount like `./app:/srv/app` resolves on the Mac to
`/Users/you/project/app`, and the VM sees the same directory at the same path,
so incus mounts it unchanged. Only `$HOME` is shared: a bind source outside it
(`/tmp`, `/Volumes/...`) is refused with an error that says so. Files are
shared with virtiofs, so an edit on the Mac is visible in the sandbox at once.
File watchers inside a sandbox are not told about Mac-side edits (Lima does
not forward inotify events by default); use polling for dev servers that watch
files.

### Users and file ownership

The VM's user has your Mac uid (usually 501). The shared home does not keep
uids the way a Linux filesystem does: Apple's virtiofs reports every file as
owned by **whoever looks at it**, and every write lands on the Mac as you. In a
sandbox, `ls -ln` shows the image's user (uid 1000) owning a bind-mounted
directory, and root sees root owning the same files; both can write them, and
whatever they create is yours on the Mac (`501:20`). `chown` inside succeeds
and changes nothing. What the Mac's own permissions refuse you is refused in
the sandbox too.

So no uid mapping is needed: `idmap: auto` sets no `raw.idmap` on macOS, and
the daemon in the VM does the same (its unit sets `ISB_BIND_CALLER_OWNED=1`).
A compose file written for a Linux host with `idmap: auto` works unchanged.
`idmap: always` still maps host 1000 to guest 1000 (the VM delegates 1000 to
root in `/etc/subuid` and `/etc/subgid`), which changes nothing for the shared
home.

One consequence: a sandbox user cannot be kept out of a bind-mounted file by
its owner or mode, since every user is the owner. Mount only what the sandbox
should be able to change, read-only (`:ro`) where it should only read.

### Ports

Published ports listen on the VM's loopback (`"8080:80"` is 127.0.0.1:8080 in
the VM), and Lima forwards every loopback TCP listener in the VM to the same
port on the Mac's 127.0.0.1 within a second or two of it appearing. So
`curl localhost:8080` on the Mac reaches the sandbox, and the same holds for a
stack's load-balanced ports. A port published on another address in the VM
(`"0.0.0.0:8080:80"`) is still forwarded only to the Mac's 127.0.0.1.

**Publish ports 1024 and above.** macOS lets an ordinary user bind a port
below 1024 only on every interface at once, so Lima could forward one only by
exposing it to the network. The machine does not forward them: `"80:80"`
works inside the VM but not from the Mac; use `"8080:80"`.

### The daemon lives in the VM

`isb serve` runs inside the machine as the Linux binary, as a systemd service
(`isb.service`), next to the bridges its balancer must reach. `isb stack ...`
and `isb tui` on the Mac talk to it through the forwarded socket. Running
`isb serve` on the Mac itself is refused with a pointer here.

`isb serve install` on macOS writes a LaunchAgent,
`~/Library/LaunchAgents/dev.isb.machine.plist`, that runs
`isb machine start` at login, so the machine, its daemon and the stacks in it
come back after a reboot. It loads the agent and waits until the daemon
answers. `--machine NAME` picks another machine. To remove it:

```sh
launchctl bootout gui/$(id -u)/dev.isb.machine
rm ~/Library/LaunchAgents/dev.isb.machine.plist
```

`isb machine rm` removes it as well when it starts that machine. The agent
runs the isb binary that installed it, by its full path: run
`isb serve install` again after moving or upgrading isb.

## Troubleshooting

- **`limactl not found`:** `brew install lima`. isb needs Lima 2.0 or later.
- **`init` waits on "incus installed and initialised":** the first boot
  installs incus with apt inside the VM. Its progress is in
  `/var/log/cloud-init-output.log`: `isb machine ssh -- sudo tail -f
  /var/log/cloud-init-output.log`. `--timeout` (default 20m) bounds the wait.
- **`cannot connect to incusd at ~/.isb/machine/isb/incus.sock`:** the machine
  is stopped (`isb machine start`) or was never created (`isb machine init`).
- **A published port does not answer on the Mac:** check it listens in the VM
  (`isb machine ssh -- ss -ltn`), and that the port is 1024 or above. Lima logs
  each forward it sets up in `~/.lima/isb/ha.stderr.log`.
- **`bind source ... is outside /Users/you`:** move the directory under your
  home, or mount a named volume instead.
- **Permission denied writing a bind mount from the sandbox:** the Mac denies
  it to you as well (check the directory's permissions on the Mac, and for
  folders like `~/Desktop` or `~/Documents`, whether macOS privacy settings
  let Lima's `limactl` access them), or the mount is `:ro`.
- **The daemon:** `isb machine ssh -- systemctl status isb` and
  `isb machine ssh -- journalctl -u isb -f`.
- **Disk:** Lima caches the Ubuntu image under `~/Library/Caches/lima`;
  `limactl prune` clears it once the machine exists.
