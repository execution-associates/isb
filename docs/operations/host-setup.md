---
title: Setting up a host
description: Install isb serve as a service, prepare the host firewall and DNS for orgs, set up the local registry, and know what the state directory holds.
order: 1
nav_title: Host setup
---

`isb up` and `isb exec` need nothing but incus. The platform (orgs, apps,
stacks, the web UI, MCP) needs the `isb serve` daemon running for good, and a
host with a default-deny firewall needs a few one-time rules so org networks
work. This page covers both, and ends with a map of everything the daemon
keeps on disk.

The short version, on a Linux host with incus:

```sh
sudo isb host setup               # firewall, DHCP/DNS for org bridges, service names
isb serve install                 # the daemon, as a systemd user service
loginctl enable-linger "$USER"    # keep it running after you log out
isb registry setup                # optional: the local registry, for builds
sudo isb host setup               # again after the registry exists: incus trusts its CA
```

The order of the first two does not matter for service names: the daemon
turns them on for every org that lacks them at each start and then once a
minute, so a `host setup` after `serve install` takes effect without a
restart (the daemon logs `turning on service names`).

## The daemon as a service

```sh
isb serve install          # writes ~/.config/systemd/user/isb.service, enables and starts it
systemctl --user status isb
journalctl --user -u isb -f
```

The installer:

- writes the unit `~/.config/systemd/user/isb.service`, which runs `isb serve`
  with `~/.config/isb/serve.env` as its environment (`EnvironmentFile=`), and
  restarts it always (`Restart=always`, after 2 s);
- writes `~/.config/isb/serve.env` (0600, in a 0700 directory) only if it is
  missing, with `ISB_SERVE_LISTEN=127.0.0.1:8092` and commented-out Cloudflare
  Access settings. `isb serve install --listen ADDR` sets a different loopback
  address and writes it into an existing file too; anything else in the file
  is yours to edit. Every daemon flag has an environment variable for this
  file ([configuration](../reference/configuration.md#daemon-flags));
- sets up the daemon's secrets key ([below](#the-secrets-key));
- reloads systemd, enables and restarts the unit, and waits (up to 30 s) for
  `http://<listen>/healthz` to answer. If it does not, the error says to look
  at `journalctl --user -u isb.service`.

Every start of `isb serve`, including the one the installer triggers, makes
sure the `default` org exists (the incus project `isb-default`, which cannot
be removed). If a default-org stack still has instances in incus' own
`default` project, the daemon logs a warning: the controller recreates that
stack in `isb-default` with new, empty volumes, and the old instances and
volumes keep running in `default` until you delete them.

It is idempotent: run it again after changing the env file or upgrading isb.
The unit runs the binary that installed it, by its full path. With the
installer that is `~/.local/bin/isb`, which `isb update` replaces in place,
so a restart picks up an upgrade; a binary at another path needs
`isb serve install` again ([Upgrading isb](upgrades.md)).

Without lingering, user services stop when you log out. The installer checks
`/var/lib/systemd/linger` and prints the `loginctl enable-linger` line when
it is off.

The daemon's user needs the incus socket, usually through the `incus-admin`
group, which is root-equivalent on that host. Treat the account the daemon
runs as accordingly.

On macOS the daemon runs inside the `isb machine` VM, and `isb serve install`
writes a LaunchAgent that starts the machine at login instead; see
[isb on macOS](../getting-started/macos.md#the-daemon-lives-in-the-vm).

### The secrets key

Where `systemd-creds` can make user credentials (systemd 256 or later), the
installer encrypts the daemon's age key (generating one if there is none)
into `~/.config/isb/isb-age-key.cred` and the unit loads it with
`LoadCredentialEncrypted=isb-age-key:%h/.config/isb/isb-age-key.cred`. It then
prints the command that removes the plaintext key, and why to add a
break-glass recipient first. Run again, it re-encrypts a plaintext key if one
exists and otherwise keeps the credential; it never makes a new key while a
credential exists.

On an older systemd the daemon keeps reading `~/.config/isb/age.txt`, and the
installer says to keep that file out of unencrypted backups. The whole story,
including break-glass recipients, is in
[Secrets](../guides/secrets.md#the-daemons-key); what to back up is in
[Backing up isb](backups.md).

### Reaching it from elsewhere

The installed daemon listens on loopback only, and refuses any other
`--listen` address apart from tailnet ones. Put a
tunnel or reverse proxy in front of it, never an open port: see
[Reach isb serve remotely](../guides/remote-access.md).

## Host firewall

A host with a default-deny firewall (ufw) drops DHCP, DNS and forwarding on
new bridges, so org networks would come up without addresses or internet.
`sudo isb host setup` once lets every org bridge (`isbbr+`) through:

- **DHCP** to the host, accepted ahead of ufw's conntrack checks: a block in
  `/etc/ufw/before.rules`, inserted before the first `ufw-before-input` rule,
  with the old file kept as `/etc/ufw/before.rules.isb-backup`. A later run
  rewrites isb's block in place.
- **Traffic between instances of one org**, in the same block. With
  `br_netfilter` loaded (Docker, Kubernetes), frames bridged within an org's
  bridge go through ufw's FORWARD chain and its routed default-deny drops
  them, all but ICMP. The rule matches `--physdev-is-bridged`, traffic that
  stays on one bridge, so traffic between two orgs (routed between bridges)
  is still denied.
- **The same for `incusbr0`**, incus' own default bridge, where isb builds
  workspace images and builder images and runs dedicated VMs: DHCP and
  same-bridge traffic in the `before.rules` block, `ufw allow in on incusbr0
  to any port 53` and `ufw route allow in on incusbr0 out on <uplink>`
  (commented `isb image builds: DNS` and `...: egress`). Without them a build
  container on `incusbr0` gets no IPv4 address; the build says so after 90
  seconds and points here, instead of waiting for a download that never
  starts.
- **DNS** to the host: `ufw allow in on isbbr+ to any port 53`.
- **Egress** through the uplink: `ufw route allow in on isbbr+ out on <uplink>`
  (`--uplink IFACE`; default the interface of the default route).
- **The ingress's tunnel listener** on each org's own bridge address:
  `ufw allow in on isbbr+ to any port 8480 proto tcp`, which a
  Cloudflare-tunnel org's cloudflared sends its requests to
  ([Domains](../guides/domains.md#cloudflare-tunnel-provider)).
- **The org-bound MCP for each org's workspace** on its own bridge address:
  `ufw allow in on isbbr+ to any port 8481 proto tcp`
  ([Workspaces](../concepts/workspaces.md#reaching-isb-from-inside-the-bridge-listener)).
  The daemon answers only the org's own subnet there.

Each rule carries a comment (`isb org bridges: DNS`, `...: egress`,
`...: tunnel ingress`, `...: workspace MCP`). ufw's routed default-deny then
keeps org bridges apart from each other and from the host's other networks.
When ufw is not active, the command says so and skips the firewall: incus'
own firewall rules already let org bridges through.

`--sandbox-egress` prepares [sandbox egress](../guides/egress.md): it lets the
egress bridges (`isbbrx*`) reach the host's proxy (`ufw allow in on
isbbrx+`, commented `isb sandbox egress: proxy`; each sandbox's ACL keeps it to
the proxy's ports) and writes `/etc/sysctl.d/61-isb-egress.conf` with
`net.ipv4.ip_unprivileged_port_start = 80`, so `isb serve` binds the proxy's
ports (443, 80 and the like) as an ordinary user.

`--public-ingress` also opens 80 and 443 (`ufw allow 80/tcp`, `443/tcp`) and
writes `/etc/sysctl.d/60-isb-ingress.conf` with
`net.ipv4.ip_unprivileged_port_start = 80`, so the daemon binds them as an
ordinary user ([Domains](../guides/domains.md)).

The same command also:

- creates the **service-name directory** (below);
- once `isb registry setup` has made the local registry, installs its CA at
  `/etc/containers/certs.d/127.0.0.1:5480/ca.crt` so incus pulls from it.

`--user USER` names the user the daemon runs as (default: the one who ran
`sudo`). `--dry-run` prints everything instead of doing it; run without root,
it prints it and exits 1.

```text
sudo isb host setup [--uplink IFACE] [--user USER] [--dry-run] [--public-ingress]
```

## Service names

Stack services in an org are reachable by name
([Orgs](../concepts/orgs.md#service-names)). The daemon writes one hosts file
per service into `/var/lib/isb/dns/<org>`, which the org bridge's dnsmasq
watches.

- `sudo isb host setup` creates `/var/lib/isb/dns`, owned by the daemon's user
  with group `incus` (dnsmasq's) and the setgid bit, mode 2750: the daemon
  writes, dnsmasq reads, nobody else can. A host without an `incus` group gets
  a world-readable 0755 directory.
- `isb org create` makes `<org>/` in it. On a host without the directory the
  org is created without service names and says so. Once `sudo isb host
  setup` has made the directory, a running `isb serve` sets `raw.dnsmasq` on
  every org that lacks it (restarting that org's dnsmasq once), the `default`
  org included; with no daemon, run `isb org create ORG` again.
- `isb org rm` deletes the org's directory.
- `ISB_DNS_DIR` moves the directory, for `isb org` and `isb serve` alike.
- When the directory is inside the daemon's state directory (a daemon running
  as root keeps its state in `/var/lib/isb`, as a server's agent does),
  `isb serve` makes the state directories on the way traversable (mode 0711:
  others may pass through, not list) so dnsmasq can reach the hosts files;
  everything in them stays 0600/0700.

## The local registry

Builds push to one OCI registry per host, run by isb in the incus project
`isb-system` and reachable only on the host's `127.0.0.1:5480`
([Builds](../guides/builds.md#the-registry)).

```text
isb registry setup [--port 5480] [--renew] [--state-dir DIR]
```

It creates or reconciles the registry (safe to repeat); restart `isb serve`
afterwards so it pushes there, and run `sudo isb host setup` so incus trusts
the registry's CA. To undo it all: `incus project delete isb-system` after
deleting its instance and volume, and remove
`/etc/containers/certs.d/127.0.0.1:5480`.

## The state directory

Everything the daemon keeps lives under one directory: `--state-dir`
(`ISB_SERVE_STATE_DIR`), default `$XDG_STATE_HOME/isb`, which is
`~/.local/state/isb` for most users (`/var/lib/isb` when there is no home).
Files are 0600 in 0700 directories and written atomically (temporary file,
fsync, rename). The host CLI's `isb user`, `isb token`, `isb audit` and
`isb history` open the same files, so run them as the daemon's user (or point
`--state-dir` at it).

Per-org data lives under an **org root**: the state directory itself for the
`default` org, `<state>/orgs/<org>/` for every other org. A few kinds are
always under `orgs/<org>/`, the default org included.

| Path | What |
|---|---|
| `isb.db` | Users, identities, passkeys, SSH keys, orgs and memberships, sessions, invitations, API and superadmin tokens (SQLite, WAL; [Identity API](../reference/identity-api.md#schema)). |
| `audit.db` | The audit log and the history, each with its own hash chain ([audit](audit.md), [history](history.md)). |
| `setup-token` | The one-time first-run setup token, for a first run nobody's front door verifies; removed once setup is done. |
| `notify.json` | Server-wide notification settings (private targets). |
| `files/<stack>/` | Where relative paths of a remote caller's stack resolve. |
| `builds/` | Images staged between a build sandbox and the registry; removed after the push. |
| `registry/` | The local registry's CA and certificate keys, and its push log. |
| `ingress/` | Caddy: `bin/caddy-<version>`, `caddy/` (certificates, the ACME account, the generated config, the internal CA under `pki/`), `caddy-home/` (the `HOME`, `XDG_DATA_HOME` and `XDG_CONFIG_HOME` Caddy is started with, whatever the daemon's own environment; certificates and the ACME account stay in `caddy/`, set by the config's `storage`), `run/admin.sock`, and `claims.json` (which org holds which hostname). |
| `templates/catalogs.json` | Template catalogs added to the built-in one. |
| `templates/logos/` | Cached template logos (a week). |
| `servers/` | A control plane's servers: `pki/` (its CA and client certificate), `servers.json`, `placement.json`, `known_hosts` ([Servers](../guides/servers.md#what-each-side-keeps)). |
| `agent/orgs.json` | On a server's agent: the orgs placed on it. |
| `<org root>/stacks/<stack>.json` | Stack definitions (secret references, never values). |
| `<org root>/apps/` | Projects, apps, deployments with their logs, previews. |
| `<org root>/sources/<app>/` | Git checkouts and per-app `known_hosts`. |
| `<org root>/backups/` | Backup destinations, schedules, runs and restore runs with logs. |
| `<org root>/jobs/` | Scheduled jobs and their runs. |
| `<org root>/volumes/` | Volume snapshot settings and runs. |
| `<org root>/workspaces/` | The workspace definition, its token encrypted to the daemon's key (`<name>.token.age`), `settings.json`. |
| `<org root>/templates/instances/` | Deployed templates. |
| `orgs/<org>/secrets/` | The org's local secrets: `<name>.age` (age ciphertext) and `<name>.json` (metadata). |
| `orgs/<org>/notify/` | Notification channels and each channel's delivery log. |
| `orgs/<org>/metrics.db` | A month of metrics history (SQLite, WAL). |

Outside the state directory the daemon also uses:

| Path | What |
|---|---|
| `~/.config/isb/serve.env` | The service's environment. |
| `~/.config/isb/age.txt`, `~/.config/isb/isb-age-key.cred` | The secrets key, plain or as an encrypted systemd credential. |
| `~/.config/isb/secrets.toml` | Break-glass recipients (`$ISB_SECRETS_CONFIG`). |
| `$XDG_RUNTIME_DIR/isb/serve.sock` | The unix socket the local CLI talks to (`$ISB_SERVE_SOCKET`). |
| `/var/lib/isb/dns/` | Service-name hosts files (`$ISB_DNS_DIR`). |
| `$XDG_STATE_HOME/isb/console/<project>/<instance>.log` | OCI instances' console output, the newest 2 MiB each. incus hands a running container's console out once (each read drains it), so every isb process of this user, the daemon, the TUI, `isb logs` and `isb up`, records what it reads here and reads from here; it ignores `--state-dir` so that they all agree. Not worth backing up. |

What to back up, and what to leave out, is in [Backing up isb](backups.md).
