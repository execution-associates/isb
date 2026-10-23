---
title: Security model and trust boundaries
description: What keeps orgs apart from each other and from the host, what a remote caller may ask for, and what isb does not protect against.
order: 6
nav_title: Security model
---

isb runs other people's and agents' code on your machines, so it is built
around a few boundaries, each enforced in a specific place. This page names
them, says what enforces each one, and says plainly where they stop. The
short version: **the host is root's, the org is the tenant, and a remote
caller is trusted to run workloads, never to own the host.**

```text
host (incus socket = root)            trusted: the daemon's user, superadmins
└── isb serve                         holds the socket; authenticates, authorizes, audits
    ├── org acme   restricted project, own bridge + ACL, quotas
    │     members administer everything in acme, nothing else
    └── org beta   same, apart from acme
```

## The host and the incus socket

Access to the incus socket is root-equivalent on that host: whoever can call
incusd can make a privileged container and mount `/`. Membership in the
`incus-admin` group is the usual way to get it, so treat it as root.

- `isb up`, `isb create` and the other local commands talk to incusd
  directly as you. They are as trusted as you are.
- `isb serve` holds the socket for everyone else. Its own unix socket is
  0600 in a 0700 directory, so only the daemon's user reaches it, and that
  caller is trusted with everything (it could run `isb` directly).
- Remote callers come in over HTTP, which `--listen` accepts only on
  loopback or on a tailnet address. Put a tunnel
  or a reverse proxy in front, never an open port
  ([Reach isb serve remotely](../guides/remote-access.md)).
- **Never mount the incus socket into a sandbox.** Anything holding it owns
  the host. Tests that need incusd are compiled in a sandbox and run on the
  host.

## Orgs

The org is the trust boundary: its members fully administer what is in it
(apps, stacks, sandboxes, volumes, secrets, including reading secret values)
and nothing crosses orgs. On a host, an org is enforced by incus itself, not
only by isb's checks ([What an org is in incus](orgs.md#what-an-org-is-in-incus)):

| Boundary | Enforced by |
|---|---|
| no privileged containers, nesting, `raw.lxc`, `raw.idmap` of root, proxy devices | the restricted incus project (nesting: [Docker needs a VM](#nesting); proxy devices: a stack's [UDP ports](orgs.md#udp-ports)) |
| no host paths but the org's bind roots | `restricted.devices.disk` and its paths |
| one network: the org's own bridge | `restricted.networks.access` |
| a uid range per instance | `security.idmap.isolated` |
| CPU, memory, disk, instance quotas | the project's `limits.*` |
| no route to private ranges (other orgs, the host's networks, the LAN, the tailnet) | the org's network ACL; exceptions only by a platform admin ([Egress exceptions](orgs.md#egress-exceptions)) |
| bridges apart from each other and the host | the host firewall's routed default-deny, after `sudo isb host setup` ([Host firewall](../operations/host-setup.md#host-firewall)) |
| one org's hostnames, images and secrets out of another's reach | isb: domain claims, `registry:` naming only the org's repositories, a secret store per org |
| host UDP ports only where a platform admin allowed them | isb: the org's [UDP ports](orgs.md#udp-ports), and once the project allows proxy devices for them, isb refuses every other proxy device in the org |

Limits, egress exceptions and domain allowlists are what keep orgs apart, so
only platform admins change them. Bind roots are host paths, set only on the
host.

Orgs share the host's kernel. When that is not enough, give the org a
separate isb in a VM or on another machine
([Several hosts](../guides/agents.md#several-hosts)).

## Nesting

Nesting keys (`security.nesting` and the `security.syscalls.intercept.*`
keys) are refused for every org instance, whoever asks; Docker inside an org
needs a VM (`type: vm`).

## The remote-spec policy

The incus socket is root on the host. A remote caller (anyone not on the
daemon's unix socket and not a superadmin, platform admins included) is
trusted to run workloads, not to own the host. Every compose file and
sandbox spec it sends is checked before anything reaches incus. An org other
than a `default` that is incus' own project is a restricted incus project,
so incus itself refuses most of what follows; that `default` org is held to
these checks by isb. Refused unless the operator allows it:

| Refused | Allowed by |
|---|---|
| `privileged: true` | `--allow-privileged` |
| `raw_config`, `raw_devices` (other than `root: {size}`, a quota), `incus_profiles`, an `idmap` other than `auto`/`none`, guest-bound ports (`bind: guest`, a guest reaching into the host) | `--allow-raw` |
| bind mounts, and any whose real path (symlinks followed) is outside the roots | `--bind-root DIR` (repeatable) |
| publishing a port on anything but loopback | `--publish-address IP` (repeatable), e.g. a tailnet address |
| reaching instances `isb serve` does not manage (exec, remove, list) | `--any-instance` |
| unix-socket listeners on the host, a different `incus_project`, `isb.*` labels, secrets read from host files | never |

And always:

- `${VAR}` in a remote caller's compose file is filled from the `vars` it
  sent, never from the daemon's environment.
- `file:` and `environment:` secret values come in the call (`secrets`, or
  `vars` for an `environment:` secret) and are stored in the org's store;
  `external`, `age` and `driver` secrets are read by the daemon.
- Relative paths resolve in `<state-dir>/files/<stack>` unless the caller
  names a `base_dir` inside a bind root.
- A sandbox a remote caller creates is labelled `isb.owner=mcp:<identity>`
  (a superadmin's `isb.owner=<source>`).

A refusal says which flag would allow it. A superadmin has no remote-spec
policy, as the unix socket has none. The web UI offers no privileged, raw or
bind-mount form fields to anyone; superadmins use the tools for those.

### Tool policy

`--allow-tools` and `--deny-tools` (names or globs such as `sandbox_*`,
comma-separated; deny wins) choose which tools remote callers see at all.
The local socket always has every tool and no policy. A superadmin has no
remote-spec policy either, but sees the tools its listener offers. For
example, `--deny-tools 'secret_*'` keeps secret values away from every remote
caller, and denying `sandbox_exec` also closes the web terminal and SSH.

## Callers and credentials

Who may do what is in [Users, roles and superadmins](access.md). What
protects the credentials themselves:

- Every bearer secret (session `isb_sess_`, API token `isb_tok_`,
  superadmin token `isb_sa_`, invitation `isb_inv_`,
  password reset `isb_rst_`, setup token `isb_setup_`) is 32 random bytes
  from the OS, shown exactly once. The database keeps only its SHA-256, and
  the row found by that hash is compared again in constant time.
- Passwords are hashed with argon2id; a failed login says nothing about
  which half was wrong and costs the same either way. Sign-in is rate
  limited per address and per IP.
- Superadmin tokens are minted only on the host, so no stolen HTTP
  credential becomes a durable superadmin one. Ambient superadmin identities
  (tailnet, Access) and an org's agent identities pass CSRF, `Origin`,
  `Content-Type` and `Host` checks ([Superadmins](access.md#superadmins),
  [Agent identities](access.md#agent-identities)).
- The first admin is created on the host, by a person a tailnet or Access
  front door verified (only someone on its superadmin list, when that list
  is set), or with a one-time setup token from `<state>/setup-token`. Either
  way, whoever merely reaches the port first cannot claim the platform.
- The org API listener on each org's bridge (port 8481) answers only peers
  in that org's own subnet, for that org alone, with an org API token
  ([The org API listener](../reference/http-api.md#the-org-api-listener)).

Details: [Identity API](../reference/identity-api.md).

## Secrets

- Values are age-encrypted at rest to the daemon's key (an encrypted systemd
  credential where the host supports it), plus any break-glass recipients
  ([Secrets](../guides/secrets.md)).
- Deployed stacks hold references (name and version), never values. Values
  never come from argv, and listings never show them.
- Delivered as files under `/run/secrets` (0400 by default), they never reach
  instance config. **On an OCI image, a secret delivered as an environment
  variable is instance config, plaintext in the incus database**, readable by
  anyone who can read the instance (`incus config show`, an incus backup or
  export, anyone with access to its incus project). `isb stack deploy` and
  `isb up` warn about each one. Deliver it with `{secret: NAME, as: file}`
  instead: the value is a 0400 file under `/run/secrets`, owned by the app's
  user and written before the app first starts, and only its path is config
  (`KEY_FILE`, the convention postgres, mariadb and many other images
  follow). On a system image a secret variable lives in a 0600 file and never
  reaches instance config either way.
- Members of an org can read its secret values (the org is the trust
  boundary); the web UI keeps them off screen behind **Reveal** for owners
  and admins. Viewers and `read`/`deploy` tokens get no secret material.
- The audit log and the history never record secret values or tool
  arguments as a whole.

## Untrusted code

- **Builds** run in a fresh sandbox in the org's own project, never on the
  host, with the source copied in as a tar (symlinks never followed on the
  host). `--untrusted` (the default for apps built from git) builds in a VM
  with its own kernel. A build sandbox has no credentials for the registry
  and no route to it ([Builds](../guides/builds.md)).
- **Git** is held to a fetch and a checkout: no hooks, no local or `ext::`
  URLs, no credential helpers, credentials only through the repository's own
  origin ([Deploy apps](../guides/deploy-apps.md#git-sources)).
- **Pull requests from forks** get no preview unless turned on, and then
  build in a VM and receive none of the app's secrets except those named
  ([Previews](../guides/previews.md)).
- **Templates** from added catalogs are third-party data: parsed and
  translated, never run on the host; a Dokploy or Coolify template that asks for
  privileges, host paths or devices is refused
  ([Templates](../guides/templates.md)).
- **Sandboxes made through the daemon** expire and are reaped when idle
  ([Sandboxes are short-lived](sandboxes.md#sandboxes-are-short-lived)).

## Sandbox egress and secrets

A sandbox's network is open unless its spec says `egress:`
([Sandbox egress and secrets](../guides/egress.md)). With it, for a container
or a VM:

- The sandbox sits on a bridge of its own that neither routes nor NATs, behind
  an ACL that drops everything but TCP to the bridge's own address on the
  ports its list uses, where `isb serve`'s proxy listens. incus enforces it on
  the host side of the virtual NIC, so nothing inside the guest can undo it.
- The proxy lets through a connection only to a listed name on a listed port,
  by the TLS server name or HTTP `Host` the client sends, and connects as the
  host resolves that name (public addresses only). The bridge's DNS has no
  upstream: it answers only the listed names, or with `dns: all` every name
  with the proxy's address, leaving the proxy to refuse. `egress: none`
  leaves the bridge with no address.
- A **secret** reaches the guest as a placeholder, and is swapped for the real
  value on the wire to its approved hosts only: the proxy terminates TLS with
  a CA made for that sandbox (its key stays on the host), and verifies the real
  host's certificate with the host's roots. The value lives in the org's store
  and the proxy's memory, never in the guest, the instance config, logs or
  tool results.
- What it does not stop: **domain fronting** through a shared front end for a
  host that is only passed through (the name is the client's word; for hosts a
  secret is approved for the proxy refuses a `Host` that differs from the
  handshake's name); code inside **using** a secret against its approved
  hosts; and any sandbox without `egress`, which keeps its open network.

## isbx

[isbx](../guides/isbx.md) runs a command line (a package install, a vendor
CLI, an installer piped to bash) in a sandbox instead of on the host:

- **What it mounts:** the project the command is run in, at its own path,
  with `.git` read-only. `/`, the home directory and its ancestors, and any
  directory that is, holds or sits inside a credential or host directory
  (`~/.ssh`, `~/.config`, `~/.local`, `/etc`, `/run`, `/var/lib/incus`, …)
  are refused, after following symlinks. The incus socket is never mounted.
- **Its network:** always an egress list, never the open network:
  closed until a package manager's preset or `--egress` adds hosts; `--net
  public` is any public name on 443 and 80, never a private address.
- **Its environment:** display variables only; anything else is named with
  `-e`. A secret is a placeholder swapped in on the wire, or (`--secret-env`)
  read on the host per run and sent in the exec request, never in a
  process's arguments.
- **Who it runs as:** uid 1000 in a container (root inside is not root
  outside), or in a VM with `--vm`.
- **What it does not stop:** the mounted project is writable, so code inside
  can change files the host later runs or trusts (`package.json` scripts,
  `.envrc`, `mise.toml` tasks, editor task files, symlinks). A container
  shares the host's kernel.

## Outbound connections

Some destinations are chosen by org members, so they must not become a way
into the host's network:

- **Notification channels and template logos** resolve every address, check
  each one, and connect only to a checked address (so DNS rebinding cannot
  swap one in). Loopback, private, link-local (cloud metadata), shared
  (tailnet) and reserved ranges are refused, in every spelling, unless a
  platform admin allows private targets for notifications
  ([Notifications](../guides/notifications.md#private-destinations-ssrf)).
  Redirects are never followed for notifications; template logos follow at
  most three, all https.
- **Backup destinations** on the daemon's own host (loopback, link-local)
  are refused unless the local CLI or a platform admin creates them.
- **cloudflared** for an org's tunnel runs inside the org, behind its ACL,
  never on the host, so a tunnel's rules cannot point at the host's
  loopback services ([Domains and ingress](../guides/domains.md)).

## The web UI

- Served from the binary; scripts only from the daemon's own origin, none
  inline, with a strict Content-Security-Policy, `X-Frame-Options: DENY` and
  `frame-ancestors 'none'`.
- Every state-changing request carries `X-Isb-Csrf: 1`, which a browser
  sends cross-origin only after a CORS preflight that isb never grants.
- Tokens in links (invitations, password resets, setup) sit in the URL
  fragment, which browsers never send to a server.
- Template logos are fetched and cached by the daemon, so a page view makes
  no request to anyone else's server.

Details: [Developing the web UI](../contributing/web-ui.md).

## Accountability

Every call that changes something, every refusal, every secret read,
sign-in, webhook delivery, terminal and SSH session is in the
[audit log](../operations/audit.md), append-only and hash-chained. The
[history](../operations/history.md) records every incus lifecycle event in
every project, including changes made outside isb, with who requested them.

## What isb does not protect against

- **A kernel exploit** in a container escapes to the host and every
  org. Use a VM (`type: vm`, `--untrusted` builds) for code you do not trust,
  and a separate isb in a VM or on another machine for an org that must
  share no kernel.
  A VM's host bind mounts are translated to the invoking user, so guest root
  cannot own files on the host ([Host directories in a
  VM](../reference/compose.md#host-directories-in-a-vm)); setuid bits set by the
  mapped id remain, as the invoking user's, unless the directory is on a
  `nosuid` mount.
- **Anyone with the incus socket**, or in `incus-admin`, owns the host.
- **The local registry has no authentication**: anything on the host that
  can open `127.0.0.1:5480` can read and write every org's images. Org
  sandboxes cannot reach it; host users and the trusted local callers of
  incus' `default` project can.
- **Plain sandboxes in incus' own `default` project** (`isb create` and
  `isb up` without `--org`) belong to no org: that project is not restricted
  and has no org network or service names, and only isb's own checks apply to
  remote callers there. Put tenants in orgs of their own.
- **Members of an org see its secrets.** Give a contractor their own org, or
  a viewer role, or a scoped token. A sandbox's [egress
  secrets](../guides/egress.md) are the exception that matters for untrusted
  code: the guest never holds the value.
- **A sandbox without `egress` has an open network**, and so does every one
  made before the setting existed. Set `egress` on anything that runs code you
  did not write.
- **VM port forwards and a stack's UDP ports are DNAT** and do not pass
  through ufw's input rules (a UDP port does meet its routed default-deny). Publish on the address you mean to expose, never `0.0.0.0` on a
  host with a public interface.
- **The org's dnsmasq runs unconfined** (no AppArmor profile) once service
  names are set up, because incus has no other way to point it at a hosts
  directory. It still drops to the `incus` user.
- **The audit chain is tamper-evident, not tamper-proof**: whoever can write
  the file can rebuild a whole chain. Copy the head somewhere else to pin it.
