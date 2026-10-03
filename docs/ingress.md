# Ingress: public hostnames for stacks

A stack service's `domains:` put it on the web: `isb serve` runs
[Caddy](https://caddyserver.com) as the HTTP(S) edge, routes each hostname
(and path) to the service's healthy replicas, and gets certificates from
Let's Encrypt. Per org, the hostnames go out through the server's public
listeners, or through the org's own Cloudflare Tunnel.

```yaml
services:
  web:
    image: docker:traefik/whoami
    deploy: {replicas: 2, update_config: {order: start-first}}
    domains:
      - {host: shop.example.com, port: 80, www_redirect: true}
      - {host: shop.example.com, path: /api, port: 3000, strip_prefix: true}
      - {host: auto, port: 80}
```

```console
$ sudo isb host setup --public-ingress     # once: ports 80/443
$ isb serve --ingress-http :80 --ingress-https :443 --acme-email ops@example.com
$ isb stack deploy shop
...
web: https://shop.example.com/ (serving, cert issued, 2 upstreams)
web: https://web-shop-default.203-0-113-7.sslip.io/ (serving, cert issued, 2 upstreams)
```

## `domains:`

| Field | Default | Meaning |
|---|---|---|
| `host` | (required) | A hostname; `*.example.com` where the org allows wildcards; `auto` for a generated name. |
| `path` | `/` | Path prefix: `/api` matches `/api` and `/api/...`. |
| `port` | (required without `redirect`) | The replicas' port. |
| `https` | `true` | HTTPS with a certificate, plain HTTP redirected to it (308). `false` serves plain HTTP. |
| `redirect` | | Answer with a 308 to this URL instead of proxying. A URL without a path keeps the request's path and query (`https://example.com`); one with a path is used as is. |
| `strip_prefix` | `false` | Remove `path` before passing the request on. |
| `www_redirect` | `false` | Also serve `www.<host>`, redirecting to `host`. |

Hostnames are DNS names (no IP addresses, no `.isb`, `.incus` or
`localhost`). A host's longest matching path wins; concrete hosts win over
wildcards. A wildcard covers one label: `*.example.com` serves
`a.example.com`, not `a.b.example.com`.

Domains are not part of a service's revision: adding or changing them
reroutes without replacing an instance.

### Generated names

`host: auto` becomes `<service>-<stack>-<org>.<a-b-c-d>.sslip.io`, which
[sslip.io](https://sslip.io) resolves to `a.b.c.d`: a working HTTPS URL with
no DNS set up. The address is `--ingress-public-ip`, or the address the
host's default route leaves from when that is a public one (true on most
cloud servers, false behind NAT; then pass it). A first label longer than 63
characters is cut and ends in a hash of the whole. Generated names are outside
every org's allowlist.

## Routing and rollouts

Each domain's upstreams are the service's replicas **in rotation**: healthy
ones (or, without a healthcheck, running ones), the same set the load
balancer and service names use. Every change in that set, every deploy and
every removal regenerates Caddy's whole config and loads it through its
admin API (`POST /load`). Caddy swaps it gracefully: requests in flight on
the old config finish (up to 10 s).

During a rollout a replica leaves rotation first; the controller waits until
the config without it is loaded and Caddy reports no requests in flight to
it (at most 10 s), and only then stops it. Between replicas Caddy spreads
requests least-connections first, retries a replica that refuses a
connection on another (twice, within 5 s), and sets a replica that failed
aside for 10 s. A service with no replica in rotation answers 503 with
`Retry-After: 5`.

Measured on titan: a 2-replica `start-first` redeploy under ~25 requests a
second over HTTPS, 1188 requests, none failed.

Backends see `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host`;
the `Host` header is the visitor's.

## Certificates

Caddy gets a certificate for every HTTPS hostname as soon as it is routed,
answering ACME's HTTP-01 challenge on the HTTP listener and TLS-ALPN on the
HTTPS one; both must be reachable from the internet on 80 and 443. It renews
them itself. `--acme-ca` picks the CA:

| `--acme-ca` | |
|---|---|
| `letsencrypt` (default) | Let's Encrypt production |
| `letsencrypt-staging` | Let's Encrypt staging: untrusted certificates, generous rate limits; use it first |
| `internal` | Caddy's own CA (its root is `<state>/ingress/caddy/pki/authorities/local/root.crt`); for tests and private networks. It is never added to the host's trust stores. |
| an `https://` URL | another ACME directory |

`--acme-email` is the ACME account's contact (expiry notices). Certificates
and the ACME account live in `<state>/ingress/caddy`, so a restart reuses
them.

A wildcard host needs a DNS-01 challenge, which this ingress does not do: on
the public listeners it is routed but gets no certificate (`cert:
unsupported`), unless `--acme-ca internal`. Put wildcards behind the
Cloudflare-tunnel provider, where Cloudflare holds the certificate.

Each domain's state in `isb stack ps` and `stack_status` (`domains`), and in
`isb ingress`:

| `cert` | |
|---|---|
| `pending` | being obtained |
| `issued` | in use (seen in Caddy's log, or found in its storage) |
| `failed` | the CA refused or the challenge failed; `message` says why, and Caddy retries with backoff |
| `unsupported` | a wildcard on an ACME CA |
| `cloudflare` | a tunnel org's HTTPS domain: TLS ends at Cloudflare |
| `none` | plain HTTP, or no HTTPS listener |

| `state` | |
|---|---|
| `serving` | routed, with replicas in rotation |
| `no-replicas` | routed; answers 503 until a replica is in rotation |
| `redirect` | a `redirect` domain |
| `conflict` | another stack holds the name (`message`) |
| `refused` | outside the org's allowlist, a `host: auto` without a public address, unreadable org settings |
| `off` | needs a listener the server does not have (an HTTPS domain without `--ingress-https`) |

Certificates issued and failed, and new conflicts and refusals, are events
on the service (the `events` tool, `/api/v1/events`).

## Who may serve a name

- **Allowlist.** A platform admin limits an org's names with `isb org create
  ORG --allow-domain example.com` ([orgs.md](orgs.md#domains)). Without a
  list any concrete name is allowed and no wildcard; a wildcard host needs a
  `*.suffix` entry.
- **First claim wins.** A name one org serves is refused to every other org,
  whatever the paths, and a wildcard counts for every name it covers. Within
  an org, a host and path belong to one service. Claims are kept in
  `<state>/ingress/claims.json` with the time they were granted, so the
  outcome does not depend on the order stacks load in; ties between new
  claims go by org, stack and service name. A deploy that would lose a claim
  is refused (`domain conflict: shop.example.com is already served by another
  org`, never naming it); a claim that loses at runtime shows as `conflict`.
  When the holder stops serving the name, the next claimant gets it.

## The edge process

- **Caddy** release 2.11.6, downloaded on first use from GitHub into
  `<state>/ingress/bin/caddy-2.11.6` and checked against the SHA-512 compiled
  into isb (amd64, arm64). `--caddy-bin` runs another binary instead.
- It is a **child of `isb serve`**: restarted with backoff (1 s doubling to
  30 s) when it exits, stopped (SIGTERM, then SIGKILL after 15 s) when the
  daemon stops, and killed by the kernel if the daemon dies without stopping
  it. A new daemon starts a new Caddy rather than adopting one; certificates
  survive in storage. So while the daemon restarts the domains are down for
  a few seconds, as published ports are.
- Its **admin API** is a unix socket in a 0700 directory
  (`<state>/ingress/run/admin.sock`, or under `$XDG_RUNTIME_DIR/isb/` when
  the state path is too long for a socket), never TCP.
- Its log (JSON) goes through the daemon: warnings, errors and certificate
  events reach the journal.
- Ports below 1024 need privileges: `sudo isb host setup --public-ingress`
  lets ordinary users bind from 80 up (`net.ipv4.ip_unprivileged_port_start`)
  and opens 80 and 443 in ufw. Or run with higher ports behind something
  else.

## Cloudflare Tunnel provider

An org that cannot (or should not) use the server's public ports gets its
names through its own [Cloudflare
Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/):

```sh
isb org create acme --ingress cloudflare-tunnel --allow-domain acme.com
isb secret create cloudflare-tunnel-token --org acme < token.txt   # a remotely-managed tunnel's token
isb secret create cloudflare-api-token --org acme < api.txt        # optional: isb manages rules and DNS
isb serve --ingress-tunnels ...                                    # or with public listeners too
```

- **cloudflared runs inside the org**, as the stack `isb-tunnel` (service
  `cloudflared`, image `cloudflare/cloudflared:2026.9.3` pinned by digest,
  token delivered as `TUNNEL_TOKEN`), unprivileged, on the org's bridge and
  behind the org's ACL. It never runs on the host: a remotely-managed
  tunnel's rules are whatever its Cloudflare account says, and on the host
  they could point at the host's own loopback services and sockets. Inside
  the org they reach what the org may reach. `isb-tunnel` is reserved; isb
  deploys it, rolls it when the token changes, and removes it when the org
  leaves the provider.
- **Caddy listens on the org's bridge address**, `http://<gateway>:8480`
  (`--ingress-tunnel-port`), with only that org's routes: the origin its
  tunnel's rules point at. `isb ingress` prints it. Other orgs cannot reach
  it (their ACLs reject private ranges, and the host firewall rule `isb host
  setup` adds only lets bridges in). An HTTPS domain redirects visitors who
  came in over plain HTTP (cloudflared's `X-Forwarded-Proto: http`); TLS
  ends at Cloudflare.
- **Without an API token**, configure the tunnel in the Cloudflare dashboard:
  a public hostname per domain, service `http://<gateway>:8480`.
- **With `cloudflare-api-token`** (permissions: Account, Cloudflare Tunnel,
  Edit; Zone, DNS, Edit), isb replaces the tunnel's ingress rules with one
  per hostname to the origin (and a final 404), and keeps a proxied CNAME
  `<host> -> <tunnel id>.cfargotunnel.com` per hostname, commented `managed
  by isb ingress`. It changes a record only if it made it, and deletes only
  its own records whose hostname went away; a record it did not make is
  reported, not touched. The account comes from the tunnel token (or
  `--cloudflare-account`); the zone is `--cloudflare-zone`, or looked up per
  hostname. It syncs when the org's hostnames change and every 10 minutes.
- Wildcards work here (Cloudflare holds the certificate), subject to the
  org's allowlist.

## Commands and tools

```text
isb ingress [--json]        the ingress_status tool: edge, routes, certificates, conflicts, tunnels
isb stack ps NAME           per service, each domain: URL, state, certificate, upstreams
```

Flags of `isb serve` (environment in brackets): `--ingress-http ADDR`
(`ISB_INGRESS_HTTP`), `--ingress-https ADDR` (`ISB_INGRESS_HTTPS`),
`--ingress-tunnels` (`ISB_INGRESS_TUNNELS`), `--ingress-tunnel-port`
(`ISB_INGRESS_TUNNEL_PORT`, 8480), `--ingress-public-ip`
(`ISB_INGRESS_PUBLIC_IP`), `--acme-ca` (`ISB_ACME_CA`), `--acme-email`
(`ISB_ACME_EMAIL`), `--caddy-bin` (`ISB_CADDY_BIN`). The ingress is on when
any of the first three is given; `ADDR` is `IP:PORT`, and `:443` means every
address.

## Limits

- One Caddy config for the whole server: a listener that cannot bind (a tunnel
  org's bridge without an address, a port taken) fails the load, and the
  previous config keeps serving; `isb ingress` shows the error.
- No DNS-01 challenge, so no wildcard certificates from an ACME CA.
- No custom certificates, basic auth or regex redirects yet.
- `isb up` ignores `domains`.
