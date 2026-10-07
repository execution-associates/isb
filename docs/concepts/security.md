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
| no privileged containers, nesting, `raw.lxc`, `raw.idmap` of root, proxy devices | the restricted incus project (nesting: [one exception](#the-docker-exception); proxy devices: a stack's [UDP ports](orgs.md#udp-ports)) |
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

## The Docker exception

Docker needs `security.nesting`, which lets a container mount `proc` and
`sysfs` and make namespaces of its own: more of the host kernel's surface,
and the same kernel every other local org shares. So an org's project
refuses nesting, and only a **superadmin** can make an exception, per org,
for its **workspace** alone (`isb org nesting ORG on`, the `org_nesting`
tool; platform admins, org owners and tokens without superadmin are
refused, and every change is in the audit log and the history).

- **The project** then allows nesting and system-call interception
  (`restricted.containers.nesting=allow`,
  `restricted.containers.interception=allow`), and the setting is recorded
  as `user.isb.allow-nesting`.
- **The workspace** gets `security.nesting=true`,
  `security.syscalls.intercept.mknod=true` (Docker makes device nodes in
  its images' layers) and `security.syscalls.intercept.setxattr=true` (the
  overlay filesystem's extended attributes, from inside a user namespace).
  It stays unprivileged, with its own uid range.
- **Nothing else** in the org gets any of them: once the project allows
  them, isb itself refuses `security.nesting` and every
  `security.syscalls.intercept.*` key in every instance config of an org
  project, unless the instance is the workspace the daemon builds. That
  mark is set by the daemon's own code and no spec, compose file or tool
  argument can carry it, so a sandbox, stack replica, app or build asking
  for nesting is refused even from a superadmin or a daemon run with
  `--allow-raw`.
- **Turning it off** is refused while the workspace runs with nesting (its
  containers live on it); with the workspace stopped, isb takes the keys
  off it and the project blocks nesting again.

The workspace and org settings pages show the warning badge **Nesting
allowed: this workspace can run Docker; more of the host kernel is
exposed.** An org that runs untrusted code and needs Docker belongs on a
separate isb inside a VM ([Several hosts](../guides/agents.md#several-hosts)),
where the kernel is its own.

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
  (a superadmin's `isb.owner=<source>`, the workspace's
  `isb.owner=workspace`).

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

- Every bearer secret (session `isb_sess_`, API token `isb_tok_`, workspace
  token `isb_ws_`, superadmin token `isb_sa_`, invitation `isb_inv_`,
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
- A workspace token is confined to its org, never shown, and served only on
  that org's own bridge to that org's subnet
  ([Workspaces](workspaces.md#reaching-isb-from-inside-the-bridge-listener)).

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
  ([Sandboxes are short-lived](workspaces.md#sandboxes-are-short-lived)).

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
  host resolves that name (public addresses only). The bridge's DNS answers
  only the listed names. `egress: none` leaves the bridge with no address.
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

### Workspace port previews

A preview ([Previews through isb](workspaces.md#previews-through-isb)) runs
whatever the workspace serves, in the viewer's browser. Served on isb's
own origin, its scripts could call isb's API as the viewer: a same-origin
request carries the session cookie and may set `X-Isb-Csrf`. So:

- **Each preview is an origin of its own**, `<port>-<workspace>-<org>`
  under `--preview-domain` or `localhost`. Previews are apart from isb and
  from each other, so one org's dev server cannot read another's preview or
  isb's pages. `x.localhost` is not even the same site as `localhost` or
  `127.0.0.1`, so a preview cannot set cookies for isb; with
  `--preview-domain`, use a domain that is not a parent of isb's own host
  (best: a registrable domain of its own), since a sibling can set cookies
  for the parent domain.
- **isb's credentials never reach the app.** The session cookie is
  host-only, so browsers do not send it to a preview host, and the proxy
  drops it anyway, with `isb_preview`, Cloudflare Access' assertion and
  `CF_Authorization` cookie (which isb would accept as the viewer), isb
  bearer tokens, and forwarding headers the client made up. The app's own
  `Set-Cookie` headers lose their `Domain` attribute, and any named like
  isb's cookies are dropped.
- **Access is a one-time link, then a cookie for that preview alone.**
  `workspace_port_open` (members and up, audited) returns a link with a
  random 256-bit token, good once and for 60 seconds, bound to that
  preview's host and the caller. Spending it sets `isb_preview` (random,
  HttpOnly, `SameSite=Strict`, host-only, 8 hours) and answers with a page
  that refreshes to the app (`Referrer-Policy: no-referrer`), so the token
  reaches neither the app nor a `Referer`. The caller's membership is
  checked again every minute; unpublishing the port ends its previews.
- **Nothing of isb's UI applies there.** Preview hosts are taken by `Host`
  before anything else on the listener: isb's pages, API and headers,
  including its Content-Security-Policy, are never served on them, and the
  app's own headers are passed as they are.
- A port published with a **host** goes through the ingress instead and
  is public, as an app's domain is, unless Cloudflare Access guards it.

Details: [Developing the web UI](../contributing/web-ui.md).

## Accountability

Every call that changes something, every refusal, every secret read,
sign-in, webhook delivery, terminal and SSH session is in the
[audit log](../operations/audit.md), append-only and hash-chained. The
[history](../operations/history.md) records every incus lifecycle event in
every project, including changes made outside isb, with who requested them.

## What isb does not protect against

- **A kernel exploit** in a container escapes to the host and every
  org, and a workspace allowed to nest ([The Docker
  exception](#the-docker-exception)) reaches more of the kernel to try. Use a VM (`type: vm`, `--untrusted` builds) for code you do not trust,
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
