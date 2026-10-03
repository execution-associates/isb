---
title: SSH and herdr
description: Plain ssh, scp, rsync, editors and herdr into any instance of an org, over isb's own authenticated connection, with nothing listening in the instance.
order: 15
---

`isb serve` carries SSH over its own authenticated websocket, so plain
`ssh`, `scp`, `rsync`, editors' remote modes and `herdr machine add` reach
any instance of an org you may exec into: its workspace, a sandbox, a
replica. Nothing listens in the instance and no port is opened on the host or
anywhere else. The keys that get in are the SSH public keys on your isb
account, so removing a key from your account locks it out everywhere at once.

```sh
isb key add ~/.ssh/id_ed25519.pub              # once: the key goes on your isb account
isb ssh-config acme/box -o ~/.config/isb/ssh_config   # then `Include` it from ~/.ssh/config
ssh box.acme.isb
scp build.tar box.acme.isb:/tmp/
herdr machine add box.acme.isb --label acme/box
```

For an org's workspace, `isb workspace ssh-config` writes the block for it
(it is `isb ssh-config` for the workspace's instance), and the web UI's
workspace **Connect** tab shows the same lines.

## How it works

`ssh` runs `isb ssh-proxy ORG/INSTANCE` as its `ProxyCommand`. That opens
`GET /orgs/<org>/api/v1/ssh?instance=NAME` on isb serve, a websocket whose
binary frames are the SSH connection's bytes. The daemon starts
`sshd -i` inside the instance through incus exec, as root, and bridges the
two. sshd does the SSH: its host key, your login, your shell, port
forwarding, `scp` and `sftp`.

Why `sshd -i` through exec rather than a TCP connection to the instance's
port 22:

- **Nothing listens.** The instance needs OpenSSH's server installed
  (`openssh-server`), not running. No address, ACL, egress rule or firewall
  is involved, so a VM, an org without egress and an instance whose network
  is down all work the same.
- **Your keys, per connection.** For each connection the daemon writes the
  account's keys at that moment to a file under `/run/isb-ssh/` and tells
  sshd to read only that file (`AuthorizedKeysFile`, with
  `AuthorizedKeysCommand`, principals and passwords off). A key removed
  from the account never opens another session, nothing has to be synced
  into the instance, and the instance's own `~/.ssh/authorized_keys`
  grants nothing here. The file is only needed while the client
  authenticates; it is removed two minutes later.
- **sshd says who.** Its log (on stderr, never on the SSH stream) names the
  guest user and the key that authenticated. The audit row records both,
  and the key's last use shows on the Account page.

The guest user is yours to pick (`User` in the config, `ssh user@...`):
anyone admitted may already exec as root in the org's instances, so SSH
adds no reach.

## Who gets in

The websocket is admitted exactly like the web terminal: as if calling
`sandbox_exec` in the org ([The web
terminal](../reference/http-api.md#the-web-terminal)). Members, admins and
owners of the org and platform admins get in; viewers, tokens whose scopes
leave out `sandbox_exec` (`read`, `deploy`) and everyone when `--deny-tools`
covers `sandbox_exec` do not. A cookie needs an `Origin` naming the site; a
bearer token and the unix socket need none.

Then the SSH layer: only the keys on the caller's own isb account. The unix
socket and superadmin tokens have no account of their own, so they name one
with `--as EMAIL` (`as=` on the websocket); anyone else may only name
themselves.

**A live session ends within 15 seconds** of any of: its key removed from
the account, the account disabled, the API token that opened it revoked or
expired, the browser session that opened it ending, the user leaving the
org or becoming a viewer in it. The client sees `isb: the SSH key that
opened this session (SHA256:...) was removed from the account`.

Limits: 64 SSH sessions per daemon, closed after 2 hours without a byte
either way (the generated config sends a keepalive every 30 seconds) and
after 24 hours in all.

Every session is in the audit log: `ssh.open`, and `ssh.close` with the guest
user, the key's fingerprint and how long it ran ([Audit
log](../operations/audit.md)).

## Keys

| | |
|---|---|
| `isb key add FILE` (`-` for stdin) `[--name N]` | add a public key (`.pub`); its comment is the default name |
| `isb key ls [--json]` | id, name, type, fingerprint, last use |
| `isb key rm ID...` | remove; sessions it opened end within seconds |
| Account page, **SSH keys** | the same, in the web UI |
| `GET/POST /api/v1/auth/ssh-keys`, `DELETE /api/v1/auth/ssh-keys/<id>` | the API (`{"public_key", "name"}`) |

With `--url` (or `ISB_URL`) and a token (`ISB_TOKEN`, or `--token-file`),
the commands act on the token's own account. Without, they open the
identity store on the host, like `isb token create`, for `--user EMAIL`
(default: the only platform admin).

Accepted: `ssh-ed25519`, `ecdsa-sha2-nistp256/384/521`, the `sk-` (security
key) variants, and `ssh-rsa` of 2048 bits or more. A key is stored as its
type and data only: `authorized_keys` options in front of it (`command=`,
`from=`) are refused, not dropped, and its comment never reaches an
instance. At most 50 keys per account. Adding and removing a key is in the
audit log (`auth.ssh_key_add` with the fingerprint, `auth.ssh_key_remove`).
An API token scoped short of `admin` cannot change keys.

## `isb ssh-config`

```sh
isb ssh-config [ORG/INSTANCE ...] [--user U] [--identity FILE] [--known-hosts FILE]
               [--as EMAIL] [--isb PATH] [-o FILE] [--url URL] [--token-file FILE]
```

Prints a `Host` block per instance (none named: every running instance in
`--org`):

```
Host box.acme.isb
  HostName box.acme.isb
  User dev
  ProxyCommand /usr/local/bin/isb ssh-proxy acme/box --url https://isb.example.com --token-file /home/me/.config/isb/token
  HostKeyAlias box.acme.isb
  UserKnownHostsFile /home/me/.config/isb/known_hosts
  StrictHostKeyChecking yes
  ServerAliveInterval 30
  ServerAliveCountMax 4
```

- **Host keys are pinned, not trusted on first use.** `ssh_host_keys` (a
  read-only tool) reads the instance's `/etc/ssh/ssh_host_*_key.pub`
  through incus, and `isb ssh-config` writes them to its own known_hosts
  file (default `~/.config/isb/known_hosts`) under the block's
  `HostKeyAlias`, replacing that alias's old lines and keeping every other
  line. Run it again after rebuilding an instance. An instance with no host
  keys (OpenSSH installed, never started) gets `StrictHostKeyChecking
  accept-new`; its first connection generates them (`ssh-keygen -A`).
- **User**: `--user`, else the instance's first ordinary user (uid 1000 and
  up, with a login shell), else root.
- **Where isb is**: `--url`/`ISB_URL` puts `--url` in the `ProxyCommand`.
  The token is read from `--token-file` (passed through) or from
  `ISB_TOKEN` where ssh runs, never written into the config. Without a URL
  the proxy uses the local daemon's unix socket, with `--as` (default: the
  only platform admin).
- `-o FILE` writes the blocks to a file; add `Include FILE` near the top
  of `~/.ssh/config`.

`isb ssh-proxy ORG/INSTANCE [--as EMAIL] [--url URL] [--token-file FILE]`
is the `ProxyCommand` itself: stdin and stdout are the SSH connection.
When the daemon refuses (not allowed, no keys, no sshd in the instance), it
prints why on stderr and exits 1, and ssh reports the connection closed.

## herdr

herdr, the terminal multiplexer for coding agents, reads `~/.ssh/config`, so
once the `Include` is there, `herdr machine add box.acme.isb` works like any
SSH machine: its panes and agents run in the instance. `isb ssh-config`
prints the herdr line for each instance on stderr, and the workspace's
Connect tab shows it for the workspace.

## Limits

- Orgs placed on a server ([Servers](servers.md)): the control plane does not
  forward SSH to them. The web terminal works there.
- `isb ssh-proxy` sends only the isb token, no Cloudflare Access
  credentials, so it cannot pass a daemon behind Access. Reach the daemon on a
  tailnet or loopback `--listen` address instead ([Reach isb serve
  remotely](remote-access.md)).
- The web terminal (`?instance=NAME`) has tabs on the workspace page but no
  reattach: closing a page or a tab ends its shell. Use SSH (and herdr) for
  sessions that should outlive a browser tab.
