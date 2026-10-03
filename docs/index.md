---
title: isb
description: Sandboxes and a small self-hosted platform on your own Linux machine, built on incus.
order: 0
nav_title: Overview
---

isb runs isolated Linux machines on a computer you control, and keeps them the
way you described them. Write down the containers or VMs you want in a small
YAML file (docker compose's format) or in Python, TypeScript or Rust, and isb
makes [incus](https://linuxcontainers.org/incus/) match it: it creates what is
missing and changes only what differs, every time you run it.

On top of that sits a self-hosted platform in the same binary: orgs that keep
teams and their agents apart, apps deployed from an image or a git repository,
databases with backups, public domains with certificates, a web UI, and an MCP
server so AI agents can do all of it under rules you set.

```yaml
# isb.yaml
services:
  web:
    image: images:ubuntu/24.04
    idmap: auto                  # files you create inside stay yours outside
    volumes: ["./site:/home/ubuntu/site"]
    ports: ["8000:8000"]         # on the host's 127.0.0.1
    user: ubuntu
    working_dir: /home/ubuntu/site
    command: python3 -u -m http.server 8000
```

```console
$ isb up        # create it (or fix only what drifted), then run `command`
$ isb down      # delete it; the host is untouched
```

## Who it is for

- **People running AI agents and untrusted code.** Give each agent or task its
  own machine with one project directory mounted, instead of your home
  directory and every credential in it. A dev server an agent starts is
  stopped when the agent goes away.
- **Developers who want a sandbox per branch.** One machine per git worktree,
  each with its own dependencies and dev server, side by side.
- **Small teams that want a Dokploy- or Heroku-style platform on their own
  server.** Projects, environments, apps, deploy webhooks, preview
  deployments, one-click templates, databases, backups and a web UI, with no
  account, no cloud and no per-minute bill.
- **Anyone hosting several tenants on one box.** Each org is its own incus
  project with its own network, quotas and secrets; an org that needs more
  gets a server or a VM of its own.

## Why it is worth it

- **A whole machine, at container speed.** incus runs system containers: init,
  users, services, a real network. One is usable in a few seconds and costs
  almost nothing idle. Switch `type: vm` and the same file gives you a VM with
  its own kernel.
- **Converges, never churns.** `isb up` changes only the difference, so a
  correct mount is never re-added and a dev server's file watching survives
  every run. `isb plan` shows the difference first.
- **Nothing hangs, nothing leaks.** Every incus call has a deadline and a
  stall names the step that stalled. A foreground `isb up` stops its sandboxes
  when whatever started it exits, even without a signal.
- **Safe defaults.** Containers are unprivileged; published ports listen on
  127.0.0.1 unless you name an address; remote callers cannot ask for
  privileged containers, host paths or raw incus config; secrets never sit in
  instance config or stack definitions.
- **Built for agents.** Every operation is an MCP tool, scoped to one org by
  its token. An org's long-lived workspace holds its own credential, so the
  agents inside administer that org and nothing else.
- **One static binary.** CLI, daemon, web UI, TUI and the SDKs' engine are the
  same program, on Linux or (through a managed VM) macOS.

## Where to start

| You want to | Read |
|---|---|
| Install isb | [Install](getting-started/install.md) |
| Run a sandbox from a YAML file | [Your first sandbox](getting-started/first-sandbox.md) |
| Run the platform and deploy an app | [Your first org and app](getting-started/first-app.md) |
| Understand the model | [Concepts](concepts/index.md) |
| Connect an AI agent | [Agents and MCP](guides/agents.md) |
| Look up a field, command or tool | [Reference](reference/index.md) |
| Run isb on a server for others | [Operations](operations/index.md) |
| Work on isb itself | [Contributing](contributing/index.md) |
