---
title: Guides
description: Task-by-task guides for running apps, data, access and agents on isb.
order: 3
---

Each guide walks through one job from start to finish, with commands that
run, and keeps the detail you need when something does not go to plan. If a
word is unfamiliar, [Concepts](../concepts/index.md) explains the model;
every flag and field is in the [Reference](../reference/index.md).

## Run apps

- [Deploy apps from an image or git](deploy-apps.md): projects, environments,
  the `.env` editor, deployments with logs and rollbacks, and push webhooks
  from GitHub, GitLab and Gitea.
- [Builds and the local registry](builds.md): turn a source directory into an
  image in a fresh sandbox, with railpack, nixpacks or a Dockerfile.
- [Preview deployments](previews.md): a preview per pull request, on its own
  URL, removed when the request closes.
- [Templates](templates.md): one-click apps from the built-in catalog or
  Dokploy's, with passwords generated and kept as secrets.
- [Coming from Dokploy](from-dokploy.md): how Dokploy's concepts map onto
  isb, and what differs.
- [Domains and ingress](domains.md): public hostnames with HTTPS, on the
  server's own ports or through an org's Cloudflare Tunnel.

## Keep data safe

- [Databases, backups and restores](databases.md): Postgres, MySQL, MariaDB,
  MongoDB and Redis with generated credentials, scheduled backups to S3, and
  restores.
- [Volumes](volumes.md): snapshots now and on a schedule, volume backups, and
  restores staged beside the live volume.
- [Secrets](secrets.md): an org's encrypted store, 1Password, break-glass
  recipients, and delivering secrets as files or variables.
- [Scheduled jobs](jobs.md): commands on a cron schedule against an app.
- [Notifications](notifications.md): deploys, health, certificates, backups
  and jobs to a webhook, Slack, Discord, Telegram or email.
- [Uptime monitoring](uptime.md): every app's public URL checked from
  outside, with incidents, uptime history, certificate warnings and a
  heartbeat for when the host dies.

## People, agents and access

- [Sign-in and accounts](sign-in.md): the first admin, invitations,
  passwords, passkeys, and GitHub, Google or OIDC sign-in.
- [Reach isb serve remotely](remote-access.md): Cloudflare Tunnel and Access
  in front of the daemon, or a tailnet.
- [Agents and MCP](agents.md): connect an agent with a token that reaches one
  org; agents in a workspace and the sandboxes they make.
- [SSH and herdr](ssh.md): plain `ssh`, `scp` and herdr into any instance of
  an org, with the keys on your isb account.
- [Workspace images and recipes](workspace-images.md): build the images
  workspaces start from, isb's default one (Claude Code, Codex, herdr,
  mise), and first-boot scripts.
- [Servers and dedicated VMs](servers.md): run orgs on other hosts, or in a VM
  of their own, from one control plane.

## Develop with sandboxes

- [A dev environment per worktree](dev-environments.md): an `isb.yaml` per
  project, one directory mounted, a dev server that dies with the session.
- [isb from code](sdk.md): the Python, TypeScript and Rust SDKs.
- [Sandbox egress and secrets](egress.md): confine an untrusted sandbox to a
  list of hostnames, and give it secrets it can use but never read.
