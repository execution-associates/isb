---
title: Your first org and app
description: Run the isb serve daemon, create the first admin and an org, deploy an app, and open the web UI.
order: 4
nav_title: First org and app
---

A sandbox lives as long as you hold it. An **app** is meant to run for
months: the `isb serve` daemon keeps it running, health-checks it,
load-balances it, rolls out new versions without downtime and rolls them
back. This page takes a Linux host with isb installed from nothing to an app
answering on a URL, then opens the web UI. (On a Mac the daemon runs in the
[isb machine](macos.md#the-daemon-lives-in-the-vm); start at step 3.)

## 1. Prepare the host, once

Each org gets its own bridge network. A host with a default-deny firewall
(ufw) drops DHCP, DNS and forwarding on new bridges (and on incus' own
`incusbr0`, where image builds run), and service names need a directory the
daemon writes. One command, as root, does all of it:

```console
$ sudo "$(command -v isb)" host setup             # --dry-run prints what it would do
```

See [host setup](../operations/host-setup.md#host-firewall) for exactly what
it changes. Run it before the daemon: the daemon then makes the `default` org
with service names. (The other order works too: a running daemon turns service
names on for the orgs that lack them within a minute of the setup, and at
every start.)

## 2. Run the daemon

```console
$ isb serve install
$ systemctl --user status isb
```

`isb serve install` writes the systemd user unit
`~/.config/systemd/user/isb.service` and its settings file
`~/.config/isb/serve.env`, sets up the daemon's secrets key, starts the
service and waits until it answers on `http://127.0.0.1:8092/healthz`. The
daemon also listens on a unix socket, which the `isb` CLI on this host uses.

Two things it may tell you:

- **Lingering is off**: user services stop when you log out. Keep it running
  with `loginctl enable-linger $USER`.
- **The unit runs a versioned path** (a mise install, which is deprecated
  because mise does not check the release signature): install with
  [the installer](install.md#2-the-binary) and run `isb serve install`
  again from `~/.local/bin/isb`.

The daemon's user needs the incus socket (the `incus-admin` group). Logs:
`journalctl --user -u isb -f`.

## 3. The first admin

isb keeps its own users. While none exists, create the first one on the
host, as the daemon's user; it is a platform admin and the owner of the
`default` org:

```console
$ isb user create you@example.com          # asks for the password (at least 12 characters)
```

Or do it in the browser. Behind Tailscale or Cloudflare Access, open `/setup`
and confirm who the front door says you are. On a plain local port, open the
setup link the daemon logged (`journalctl --user -u isb | grep setup`); its
token is also in `~/.local/state/isb/setup-token`. See [the first
admin](../guides/sign-in.md#the-first-admin).

## 4. An org

An org is the trust boundary: its own incus project, network, quotas and
secrets. Its members administer what is in it and nothing outside it.

```console
$ isb org create acme --cpus 4 --memory 8GiB
$ isb org show acme
```

Every later command takes `--org acme` (or set `ISB_ORG=acme`). Without it,
platform commands work in the `default` org, which always exists once the
daemon has started (the incus project `isb-default`). See
[orgs](../concepts/orgs.md).

## 5. A project and an app

Apps live in a project's environment (`production` unless you add others),
and each environment runs as one stack:

```console
$ export ISB_ORG=acme
$ isb project create shop
$ isb app create web --project shop --image docker:traefik/whoami \
    --port 80 -p 127.0.0.1:8080:80 -e GREETING=hello --deploy
created app web: service web.shop-production
...
deployment 1 done
$ curl -s localhost:8080 | head -3
```

`--deploy` deploys right away and follows the deployment's log until it is
done. The image is pinned to its digest at deploy, so a moved tag never
changes a running app behind your back. `-p 127.0.0.1:8080:80` publishes the
app on the host's loopback, load-balanced over its healthy replicas.
(`docker:` images need `skopeo` on the host.)

Day to day:

```console
$ isb app ls
$ isb app env web > web.env && $EDITOR web.env && isb app env-set web web.env --deploy
$ isb app update web --replicas 2 --deploy
$ isb app deployments web
$ isb app rollback web               # the last good deployment before this one
$ isb stack ps shop-production       # the stack underneath: replicas, health, ports
```

An app from a git repository is the same with `--git URL --ref main`; the
daemon builds it in a fresh sandbox. See [deploying apps](../guides/deploy-apps.md).

## 6. A public URL

To serve the app on a hostname with a certificate, the daemon runs an
ingress (Caddy) on the host's 80 and 443, or an org uses its own Cloudflare
Tunnel:

```console
$ sudo "$(command -v isb)" host setup --public-ingress    # open 80/443, let the daemon bind them
$ echo 'ISB_INGRESS_HTTP=:80' >> ~/.config/isb/serve.env
$ echo 'ISB_INGRESS_HTTPS=:443' >> ~/.config/isb/serve.env
$ systemctl --user restart isb
$ isb app create hello --project shop --image docker:traefik/whoami --port 80 --domain shop.example.com --deploy
```

The name's DNS must point at the host. Without a name of your own, `--domain
auto` gets a generated `*.sslip.io` name for the host's public address. See
[domains and ingress](../guides/domains.md).

## 7. The web UI

Open `http://127.0.0.1:8092` in a browser on the host (or through an SSH
tunnel: `ssh -L 8092:127.0.0.1:8092 host`) and sign in. Pick **acme** in the
org switcher: **Projects** shows `shop` with the app, its deployments, logs,
metrics and a terminal. See [the web UI](web-ui.md).

The daemon listens only on loopback. To reach it from elsewhere, and to let
agents in over MCP, put Cloudflare Tunnel and Access or a tailnet in front:
[remote access](../guides/remote-access.md) and [agents and MCP](../guides/agents.md).

## A compose file instead

Apps sit on top of **stacks**, which take an `isb.yaml` directly: the file
you develop with, plus `deploy:` for replicas and rolling updates.

```yaml
# isb.yaml
services:
  api:
    image: docker:traefik/whoami
    ports: ["127.0.0.1:8081:80"]
    deploy:
      replicas: 3
      update_config: {order: start-first}   # no gap during a rollout
```

```console
$ isb stack deploy demo
$ isb stack ps demo
$ isb stack rollback demo
$ isb stack rm demo
```

See [stacks](../concepts/stacks.md).
