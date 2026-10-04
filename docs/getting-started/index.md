---
title: Getting started
description: Install isb, make your first sandbox, then run your first org and app on the isb serve daemon.
order: 1
---

isb is two things in one binary, and you can use either without the other:

- **Sandboxes you describe.** An `isb.yaml` file (docker compose's format)
  lists the containers or VMs you want; `isb up` makes incus match it. This
  is for dev environments, agents and throwaway test machines, and needs
  nothing but incus.
- **A platform you run.** The `isb serve` daemon keeps apps running for
  months: orgs that keep tenants apart, apps from an image or a git
  repository, deployments with rollback, databases with backups, domains
  with certificates, a web UI and MCP tools for agents.

Start with the first two pages either way, then pick a path.

| Step | Page | You get |
|---|---|---|
| 1 | [Install](install.md) | incus, the `isb` binary, and the SDKs if you want them |
| 2 | [Your first sandbox](first-sandbox.md) | an `isb.yaml`, `isb up`, `isb exec`, `isb down` |
| 3 | [Your first org and app](first-app.md) | the daemon, an admin account, an org, an app answering on a URL |
| 4 | [The web UI](web-ui.md) | a tour of what the browser UI shows and does |

On a Mac, isb runs incus in a Linux VM it manages: read
[isb on macOS](macos.md) after installing.

When you know what you want to build, the [concepts](../concepts/index.md)
explain the model (orgs, apps, stacks, workspaces, placement, security) and
the [guides](../guides/index.md) walk through each task.
