---
title: Troubleshooting
description: Symptoms you may meet with isb, what causes them, and the fix, from a stalled create to a notification that never arrives.
order: 7
---

isb tries to fail with a message that names the step and the reason. This
page collects the messages and symptoms people meet most, grouped by where
they show up, with the cause and what to do. When nothing here fits, the
daemon's journal (`journalctl --user -u isb -f`) and the
[history](history.md) of the object involved (`isb history NAME`) usually
say what happened.

## Sandboxes and `isb up`

| Symptom | Cause | Fix |
|---|---|---|
| `isb exec` exits **125** | isb itself failed, not the command: no such sandbox, incusd unreachable. The command's own status is passed through otherwise. | Read the message on stderr; check `isb ls` and that incusd answers (`incus info`). |
| `cannot connect to incusd at ...` | The socket isb uses is missing or not yours: `--socket`, else `$INCUS_SOCKET`, else `$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket` (on macOS `~/.isb/machine/isb/incus.sock`). | Start incusd, or join the `incus-admin` group, or point `--socket` at the right path. On a Mac, `isb machine start`. |
| `create instance web stalled: operation ... still Running after 600s` | An incus operation ran past its deadline (often a slow image download). Every incus call has one; a stall is reported as the step, never a silent hang. | Retry with more time: `isb --create-timeout 20m up`. A half-created instance carrying this call's token is deleted; anyone else's is left alone. |
| `isb up` returns but the sandbox stopped | A foreground `isb up` stops its sandboxes when every `command` exits, on Ctrl-C, or when the process that started it goes away (exit 129), even without a signal. A wrapper script that backgrounds `isb up` and exits counts as going away. | Use `isb up -d` in scripts. For a dev server that should die with your session, run plain `isb up` as a background task of that session ([Your first sandbox](../getting-started/first-sandbox.md)). |
| Exit **141** from `isb up` | The reader of its stdout went away (a closed pipe). | Keep the pipe's reader alive, or use `-d`. |
| `... (it stopped while getting ready; see incus info --show-log NAME)` | The instance stopped during the readiness checks. isb starts a stopped instance once more after 30 s (a guest-initiated reboot, common on a VM's first boot with cloud-init, sometimes does not complete); if it stops again, or the start fails, readiness fails at once. | `incus info --show-log NAME` shows why the guest stopped. Fix the image or `raw_config`, then `isb up` again. |
| `not_ready`: a readiness check did not pass in time | The shared deadline ran out: `ready_timeout`, default 60 s for a container, 300 s for a VM. | Check the named check by hand (`isb exec NAME -- ...`); raise `ready_timeout`, or fix the check. |
| `plan` shows a `note:` about the image, storage, profiles or type | These are fixed at creation and never changed on an existing instance. | Recreate: `isb down` then `isb up`. |
| A removed field, label or variable is still on the instance | isb never unsets config keys; removing a field leaves the old key. | Unset it by hand (`incus config unset`), or recreate. |
| A dev server's file watching stops after `isb up` | A disk device was replaced (a remount kills inotify watches). A correct mount is never touched, so this happens only when the mount changed. | Check `isb plan` before `isb up` on a sandbox someone is using. |
| Files written inside a bind mount are owned by nobody, or not writable | The uid map: `idmap: auto` maps 1000 to 1000 only where `/etc/subuid` needs it. | Use `idmap: auto` (or `always`), and run as the uid that owns the checkout ([isb.yaml reference](../reference/compose.md#idmap)). |
| A named volume mounts empty | Seeding from the image needs a container and incus' `disk_initial_copy` extension, and acts only on first use; `:nocopy` turns it off. | Set `owner=` on the mount, or populate the volume. |

## Virtual machines

| Symptom | Cause | Fix |
|---|---|---|
| A published port on a VM does not answer on `127.0.0.1` | incus proxies into a VM only in NAT mode, and a NAT listen on host `127.0.0.1` does not work (`route_localnet` is off on the bridge). | Publish on another address you mean to expose, such as a tailnet IP: `"100.64.0.5:8080:80"`. |
| `bind: guest` refused on a VM | Guest-bound proxies are container-only. | Reach the host service another way (its address on the bridge). |
| A VM port answers from the internet despite ufw | NAT forwarding is DNAT and does not pass through a host firewall such as ufw. | Never listen on `0.0.0.0` on a host with a public interface; name the address. |
| A VM proxy needs a static IP | incus older than 7.0.1 cannot find the VM's address by itself. | Upgrade incus, or set `ipv4.address` on the VM's NIC (`raw_devices`) and use it in `connect`. |
| File watchers in a VM miss host edits | Host bind mounts are shared over virtiofs, which does not deliver inotify events for host-side edits. | Use polling in the dev server or test watcher. |
| `exec` hangs at first on a VM | It goes through the incus agent, which starts some time after the VM. | Keep the default `ready` (`[running, agent]`). |

## macOS

| Symptom | Fix |
|---|---|
| `limactl not found` | `brew install lima`; isb needs Lima 2.0 or later. |
| `init` waits on "incus installed and initialised" | The first boot installs incus with apt inside the VM. Follow it: `isb machine ssh -- sudo tail -f /var/log/cloud-init-output.log`. `--timeout` (default 20m) bounds the wait. |
| `cannot connect to incusd at ~/.isb/machine/isb/incus.sock` | The machine is stopped (`isb machine start`) or was never created (`isb machine init`). |
| A published port does not answer on the Mac | Check it listens in the VM (`isb machine ssh -- ss -ltn`) and that it is 1024 or above: macOS lets an ordinary user bind lower ports only on every interface, so they are not forwarded. Lima logs each forward in `~/.lima/isb/ha.stderr.log`. |
| `bind source ... is outside /Users/you` | Only `$HOME` is shared with the VM. Move the directory under your home, or mount a named volume. |
| Permission denied writing a bind mount | The Mac denies it to you as well (check the directory's permissions, and for `~/Desktop` or `~/Documents` whether macOS privacy settings let Lima's `limactl` access them), or the mount is `:ro`. |
| `isb serve` refused on the Mac | The daemon runs inside the machine. `isb serve install` writes the LaunchAgent that starts it. |
| The daemon | `isb machine ssh -- systemctl status isb` and `isb machine ssh -- journalctl -u isb -f`. |
| Disk filling up | Lima caches the Ubuntu image under `~/Library/Caches/lima`; `limactl prune` clears it once the machine exists. |

More in [isb on macOS](../getting-started/macos.md).

## The daemon

| Symptom | Cause | Fix |
|---|---|---|
| `isb serve install` fails with `.../healthz did not answer 200` | The service did not come up within 30 s. | `journalctl --user -u isb.service` says why (a bad flag in `serve.env`, a key that cannot be read, a port in use). |
| The service stops when you log out | Lingering is off. | `loginctl enable-linger $USER`. |
| The unit still runs the old version after an upgrade | The unit runs the binary by its full path. | `isb serve install` again ([Upgrading isb](upgrades.md)). |
| `isb stack ...` cannot reach the daemon | The CLI talks to `$ISB_SERVE_SOCKET`, else `$XDG_RUNTIME_DIR/isb/serve.sock`. | Check `systemctl --user status isb`, and that the CLI runs as the daemon's user with the same `XDG_RUNTIME_DIR`. |
| Anonymous calls answer 401 | Every HTTP caller must sign in: a session, an API token, Access, or a superadmin source. | Make a token (`isb token create`) or sign in. `--allow-unauthenticated` is for local testing only. |
| `... is schema version N, newer than this isb understands; upgrade isb` | A newer isb has migrated `isb.db` or `audit.db`; databases are never downgraded. | Run the newer isb, or restore the state directory from before the upgrade. |
| Store secrets cannot be read by `isb up` | With the key only in the daemon's systemd credential, `isb up` reads store secrets through the running daemon. | Start the daemon, or make the key available to `isb up` ([Secrets](../guides/secrets.md#the-daemons-key)). |
| A warning about no break-glass recipient at start-up | Losing the key would lose every secret. | Add one ([Backing up isb](backups.md#the-secrets-key)). |
| A password reset link never arrives | No mailer is configured: the daemon writes the reset link (or token) to its journal. | Read it from `journalctl --user -u isb` and hand it over. |
| Provider sign-in returns `/login?error=CODE` | See the codes in [Sign-in](../guides/sign-in.md#signing-in-with-github-google-or-oidc); details of a `provider_error` are in the journal. | |
| `isb ssh-proxy` fails behind Cloudflare Access | It sends only the isb token, not Access credentials. | Reach the daemon on a tailnet or loopback `--listen` address ([SSH](../guides/ssh.md)). |

## Stacks and apps

| Symptom | Cause | Fix |
|---|---|---|
| A service is `paused` | A rollout failed with `failure_action: pause`; the message says why. | Fix the cause, then deploy again. |
| A service is `failing` | Something the daemon keeps retrying: an image that will not pull, a replica that stays unhealthy. | `isb stack ps NAME` shows the last probe output; `isb stack logs NAME SERVICE` the replicas' output. |
| An app or stack is refused with `image ... not found on Docker Hub` | The registry has no such image (or none for this host's platform). A colon where a slash was meant is the usual slip: `docker:traefik:whoami` is the image `traefik` with the tag `whoami`. | Use the suggested name, or check it on the registry ([Image references](../guides/deploy-apps.md#image-references)). |
| A service is `failing` with `image ... not found` | Its image went away after it was deployed (a deleted tag or repository). Retries back off from 5 minutes to an hour. | Push the image again, or point the app at one that exists and deploy. |
| A service is `waiting` | Its `depends_on` is not met. | Look at the dependency's state. |
| Published ports pause during a daemon restart | The load balancer and ingress live in the daemon; apps do not. | Expected; they return within seconds. |
| Apps cannot reach each other by name | Service names need an org with its own network: the `default` org on a host whose incus `default` project held workloads first has none. On a fresh org, the host may lack the service-name directory. | Use an org of its own; run `sudo isb host setup`, after which a running `isb serve` turns names on within a minute (`isb org create ORG` again does it without the daemon) ([Setting up a host](host-setup.md#service-names)). |
| A new org's instances get no address or no internet | A default-deny firewall drops DHCP, DNS and forwarding on new bridges. | `sudo isb host setup` ([Host firewall](host-setup.md#host-firewall)). |
| A workspace image or builder image build stops with `got no IPv4 address on incusbr0` | The same firewall drops DHCP and forwarding on incus' own bridge, where those builds run. | `sudo isb host setup`, then build again. |
| `isb serve` or `isb host setup` warns that incus is too old for OCI images | The incus package is older than 6.3 (Ubuntu 24.04's is 6.0), so `docker:` images cannot run. | Install incus from the Zabbly stable repository ([Install isb](../getting-started/install.md#1-incus)). |
| Instances of one org cannot reach each other on a Docker or Kubernetes host | With `br_netfilter` loaded, bridged frames go through ufw's FORWARD chain. | `sudo isb host setup` adds the same-bridge rule. |
| A deploy fails with `domain conflict: ... is already served by another org` | The first org to claim a name keeps it. | Use another name, or have the other org stop serving it. |
| A domain shows `cert: failed` | The CA refused or the challenge failed; `message` says why, and Caddy retries with backoff. | Make sure ports 80 and 443 reach the host from the internet. Try `--acme-ca letsencrypt-staging` first to stay under rate limits. |
| A domain shows `cert: unsupported` | A wildcard host on an ACME CA needs DNS-01, which the ingress does not do. | Use the Cloudflare Tunnel provider for wildcards. |
| A domain shows `off` | It needs a listener the daemon does not have (an HTTPS domain without `--ingress-https`). | Start the daemon with the listener ([Domains](../guides/domains.md)). |
| A domain shows `refused` | Outside the org's allowlist, a `host: auto` without a public address, or unreadable org settings. | Ask a platform admin to allow the domain, or pass `--ingress-public-ip`. |
| Pushes to the forge never deploy behind Cloudflare Access | Access blocks the webhook path before it reaches isb. | Add an Access bypass policy for `/api/v1/webhooks/*`; each request is signed, and isb serves that path ahead of its Access check ([Deploy apps](../guides/deploy-apps.md#webhooks)). |
| A webhook answers 401 | A missing or wrong signature, unknown org or unknown app all answer 401 alike. | Check the secret (`isb app webhook NAME`) and the sender's settings. |
| A fork's pull request gets no preview | `ignored ... previews for forks are off (previews.forks)`. | Turn on `forks` in the app's previews if you accept the risk ([Previews](../guides/previews.md#pull-requests-from-forks)). |
| A build is slow the first time | The builder image and the build cache are prepared on first use. | Rebuilds reuse both ([Builds](../guides/builds.md#build-cache)). |
| A MongoDB 8.0 or 8.2 database will not start | MongoDB 8.0 and 8.2 refuse to start on Linux 6.19 and newer (SERVER-121912). | Use another version, or a host with an older kernel. |
| A Postgres database will not start after a version change | A new major version over old data is the engine's business; Postgres refuses. | Restore a backup into a new database with the new version. |
| A sandbox disappeared | Sandboxes made through `isb serve` expire (24 h) and are deleted when idle (2 h); the history records `sandbox.reaped` with the reason. | Extend it (`sandbox_extend`, `isb workspace extend`) or set longer org defaults ([Workspaces](../concepts/workspaces.md#sandboxes-are-short-lived)). |

## Notifications

| Symptom | Cause | Fix |
|---|---|---|
| A delivery fails with `... is a private address ...; private targets are off (a platform admin can allow them)` | Destinations are checked against the SSRF policy: loopback, private, link-local, shared (tailnet) and reserved ranges are refused by default. | Use a public endpoint, or have a platform admin run `isb notify settings --allow-private-targets true`. |
| A delivery fails at once with a 3xx | Redirects are never followed. | Point the channel at the final URL. |
| A delivery is `dropped` | The channel's queue (100) was full. | Fix or disable the slow destination. |
| Deliveries queued before a restart never arrive | The queue is in memory and starts over with each daemon run. | Expected; the delivery log shows what was sent. |
