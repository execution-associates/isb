---
title: Sandbox egress and secrets
description: Confine a sandbox's network to a list of hostnames, and give it secrets it can use but never read, for running untrusted code in a container or a VM.
nav_title: Egress and secrets
order: 19
---

By default a sandbox reaches the whole internet. For code you did not write
(a plugin, a package's install script, an agent's output) that is too much:
it can send anything to anyone. `egress:` turns it around. The sandbox gets no
network of its own, only a proxy `isb serve` runs on the host, and the proxy
lets through connections to the hostnames you list and nothing else. A secret
can be given the same way: the sandbox holds a placeholder, and the real value
is put on the wire only towards the hosts you approved for it.

```yaml
services:
  plugin:
    image: images:ubuntu/24.04/cloud
    type: vm                       # untrusted code: its own kernel
    egress:
      allow:
        - registry.npmjs.org       # port 443 unless you say otherwise
        - "*.githubusercontent.com"
        - db.example.com:5432
      secrets:
        - {env: API_TOKEN, secret: acme-api-token, hosts: [api.example.com]}
```

```sh
isb create plugin -i dev-base --egress registry.npmjs.org --egress '*.example.com:8443'
isb create plugin -i dev-base --egress none                          # no network at all
isb create plugin -i dev-base --secret API_TOKEN=acme-api-token@api.example.com
```

## The rules

`egress:` is one of three shapes, in a compose file, a spec sent through the
[SDKs](sdk.md) and the [rpc protocol](../reference/rpc.md), and the
`sandbox_create` tool:

| Form | Means |
|---|---|
| omitted | open network, as before |
| `none` | no network at all: no address, no DNS, no route |
| a list of `host[:port]` | only those; `*.example.com` covers every name below it (not `example.com` itself: list that too); the port is 443 unless given |
| `{allow: [...], secrets: [...]}` | the list, plus secrets |

- Hosts are **names**. An IP address cannot be allowed, and neither can a
  wildcard with a single label (`*.com`). Everything else, public or private,
  is refused, including a direct connection to an IP address.
- A list that is empty means `none`.
- A name that resolves to a private, loopback or link-local address is
  refused, whatever the list says: allowing a name never reaches the host's
  own network. An operator who wants one such name reachable pins it with
  `isb serve --egress-pin NAME=IP[:PORT]`.
- Existing sandboxes, and every sandbox without `egress`, keep their open
  network. **Set `egress` for anything untrusted.** The network of a sandbox
  is fixed when it is created in practice: recreate it to add or remove
  `egress`. Changing the list or the secrets of one that has `egress` is
  live, within a couple of seconds, with no restart.
- `egress` needs `isb serve` running: the proxy is its. A sandbox made while
  it is down comes up with a network that goes nowhere (connections are
  refused) until the daemon runs, and works from then on, also after the
  daemon restarts.

## What it guarantees

Say it exactly, because it is a security boundary:

1. **The guest can send packets only to the proxy.** Its network is a bridge
   of its own that does not route or NAT, with a network ACL that drops
   everything but TCP to the bridge's own address on the ports your list
   uses. Another sandbox, the host's other addresses, the LAN and the
   tailnet are unreachable. This holds for containers and VMs alike, because
   it is enforced by incus on the host side of the sandbox's virtual NIC, not
   inside the guest. IPv6 is off on the bridge.
2. **The guest resolves only the allowed names.** The bridge's DNS server
   has no upstream: it answers the listed names (with the proxy's address) and
   returns NXDOMAIN for everything else, so it cannot be used to leak data
   through queries. With `egress: none` the bridge has no address at all.
3. **The proxy lets a connection through only if the name the client says is
   on the list, on the port it connected to.** The name is the TLS server
   name (SNI) of a TLS connection, or the `Host` of a plain HTTP request. The
   proxy then connects to that name as *the host* resolves it, never to an
   address the guest chose. TLS and HTTP are passed through byte for byte.
   The proxy serves only the guest: a connection from the host itself
   (its own address, loopback) is dropped, so a local user cannot borrow the
   proxy, or a sandbox's secrets, by connecting to its bridge.
4. **A secret's real value never enters the guest** and is put on the wire
   only towards the hosts approved for it, and only over TLS the proxy
   verified (below).

What it does not guarantee:

- **The name is the client's word.** The proxy does not decrypt TLS it only
  passes through, so it trusts the SNI or `Host` the client sends. Code that
  can reach one allowed host on a shared front end (a CDN) can ask that front
  end for another site it hosts (domain fronting). For hosts a secret is
  approved for, the proxy closes this: it decrypts the connection and refuses
  a request whose `Host` is not the host the TLS handshake named. Allow
  hosts you would trust with the traffic.
- Code inside the sandbox can still **use** a secret against its approved
  hosts: call the API it unlocks, including to send data out through it. The
  secret protects the value, not the account's powers.
- A protocol that shows no name is held to one host per port: see
  [Other protocols](#other-protocols).

## Secrets that never enter the guest

```sh
printf %s "$TOKEN" | isb secret create acme-api-token       # in the org's store
isb create plugin -i dev-base --secret API_TOKEN=acme-api-token@api.example.com
```

`--secret NAME[=SECRET]@host1,host2` (`secrets: [{env, secret, hosts}]`)
means: the guest sees the environment variable `NAME` holding a **placeholder**
(`isb_placeholder_...`, derived from the sandbox and the variable, stable
across restarts); the real value is the org [secret](secrets.md) `SECRET`
(default: `NAME`). The hosts of every secret are allowed too, on port 443
unless given.

- **Where the value lives.** In the org's secret store, and in the proxy's
  memory for a connection. It is never written into the guest, the instance
  config, logs or tool results, and the proxy never prints it. A sandbox
  outside every org reads the default org's store. The serve tools refuse a
  sandbox that names a secret the org does not hold.
- **How it reaches the host.** For a connection whose server name is a host
  the secret is approved for, the proxy terminates TLS with a certificate for
  that host from a **CA made for this sandbox** (its key stays in the
  daemon's state directory, `egress/<network>/ca.key`, 0600; only the
  certificate goes into the guest), opens its own TLS connection to the real
  host, **verifying the real certificate against the host's roots** (plus any
  `--egress-ca`), and swaps the placeholder for the value in the request
  line and in every header, including inside `Basic` credentials. Anything the
  host sends back that contains the value (an echo, an error message) is
  turned back into the placeholder before the guest sees it. A host that is
  not approved for the secret is never intercepted: the connection is
  passed through, with the placeholder in it unchanged.
- **Trusting the CA.** At create, isb installs the CA certificate in the
  guest's system store (Debian, Alpine and Red Hat families) and points the
  runtimes that keep their own settings at it: `SSL_CERT_FILE`,
  `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE` and `GIT_SSL_CAINFO` name
  `/etc/isb/egress-ca-bundle.pem` (the system roots and the CA), and
  `NODE_EXTRA_CA_CERTS` names `/etc/isb/egress-ca.crt`. A runtime with a
  store of its own (a Java keystore, a bundled `certifi` that ignores the
  variables) has to be told to trust `/etc/isb/egress-ca.crt`.
- **What it does not do.** A client that pins the host's certificate fails.
  Only HTTP/1.1 is spoken to intercepted hosts (the proxy offers no `h2`, so
  gRPC fails). The request line and headers are rewritten; a request body is
  forwarded as it is. A response that comes back compressed cannot be
  scrubbed (the proxy asks for `identity`). A WebSocket upgrade is tunnelled
  without scrubbing. Plain HTTP to an approved host passes the placeholder:
  a secret never travels unencrypted. A value with line breaks cannot be a
  header and is refused (a trailing newline, as a file leaves, is dropped).

## Other protocols

The proxy reads a name from TLS and HTTP. For a connection that shows none (a
server speaks first, as SMTP and Postgres do; or the protocol is not TLS or
HTTP), it falls back to the **one exact host** the list names on that port: if
`db.example.com:5432` is the only entry on 5432, a client that connects to
`db.example.com:5432` is sent there. With two entries on a port, or a
`*.suffix` entry, such a connection is refused. UDP and ICMP never leave the
sandbox, so QUIC clients fall back to TCP.

## Setting up the host

The proxy listens on each sandbox's bridge address, which a default-deny
firewall blocks, and on ports the list names:

```sh
sudo isb host setup --sandbox-egress
```

This lets the sandbox egress bridges (`isbbrx*`) reach the host's proxy in ufw
(`ufw allow in on isbbrx+`, commented `isb sandbox egress: proxy`; the
sandbox's ACL is what keeps it to the proxy's ports), and lets unprivileged
users bind ports 80 and up (`net.ipv4.ip_unprivileged_port_start=80`, in
`/etc/sysctl.d/61-isb-egress.conf`), so a proxy run as you can listen on 443
and 80. Ports below 80 are served only by a daemon with the capability. DHCP
for the bridges comes from the [rules for org bridges](../operations/host-setup.md#host-firewall)
that the same command installs. Without ufw nothing needs doing. The daemon's
operator flags are in [Configuration](../reference/configuration.md#sandbox-egress).

## Looking at it

`isb inspect` and the `sandbox_get` tool show the sandbox's config: its
`user.isb.egress` key holds the policy (the hosts and, for secrets, the
variable, the store name, the hosts and the placeholder, never a value). The
sandbox's bridge is `isbbrx<hash>` and its ACL `isbx-<hash>` in incus. Denied
connections are logged by `isb serve`, once per target per half minute
(`egress <project>/<sandbox>: denied host:port: reason`).

When a sandbox is removed its bridge, ACL and CA go with it; `isb serve`
removes any left behind by a sandbox deleted some other way, ten minutes after
the network was made.

## Who makes the sandbox

`isb create`, `isb up`, the SDKs and the daemon's tools all set up the same
thing in incus; the daemon's proxy notices a new egress network within about a
second. The CLI and the daemon share the sandbox's CA through the state
directory (`$XDG_STATE_HOME/isb`, or `--state-dir`), so run them as the same
user, as the installed service does. On macOS, where the daemon runs in the
`isb machine` VM, create sandboxes with secrets through the daemon's
`sandbox_create`.

## Host directories in a VM

A closed network is half of running untrusted code in a VM; the other half is
what it can write to the host. A host directory bind-mounted into a VM is shared
over virtiofs, and isb makes incus translate its ids on the host side: only one
guest uid and gid (the service user, root when `user:` is unset) can touch it,
and they land on the host as the user running isb. Any other guest id, root
included, is refused when it creates or chowns a file or makes a device node, so
the sandbox cannot plant a root-owned file on the host. A setuid bit can still
be set on a file the mapped id owns, but that file is the invoking user's, so
for real separation put the directory on a `nosuid` mount or mount it `:ro`.
This needs incus 7.5 or later; isb refuses a VM with host mounts on an older
one. Details and the `idmap` forms:
[Host directories in a VM](../reference/compose.md#host-directories-in-a-vm).

## Limits

A proxy holds at most 256 connections at once, a connection idle for ten
minutes is closed, and a sandbox may name at most 256 hosts and 16 secrets.
Each egress sandbox costs one bridge, one `dnsmasq` and a handful of listeners.
