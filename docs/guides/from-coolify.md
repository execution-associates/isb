---
title: Coming from Coolify
description: How Coolify's servers, projects, resources and one-click services map onto isb, and how to deploy its templates here.
order: 5.5
---

Coolify and isb both turn a server into a place where you deploy apps,
databases and one-click services from a web UI, with HTTPS in front. The
difference is underneath: Coolify runs everything it deploys on the server's
Docker daemon (and its proxy), where isb gives each org its own restricted
incus project, with unprivileged system containers (or VMs), a network and
quotas ([Security model](../concepts/security.md)), and agents operate it
through each org's MCP server ([Agents and MCP](agents.md)).

## The model

| Coolify | isb |
|---|---|
| Team | An [org](../concepts/orgs.md): an incus project with its own bridge, network ACL and quotas; members are `owner`, `admin`, `member` or `viewer` ([Users, roles and superadmins](../concepts/access.md)). |
| Server | The host `isb serve` runs on. Several servers each run their own isb, connected to the same agent over MCP ([Several hosts](agents.md#several-hosts)). |
| Project, environment | The same: a [project](deploy-apps.md#projects-and-environments) with environments. Each runs as one stack, `<project>-<env>`. |
| Application (git or image) | An [app](deploy-apps.md): `--image REF` or `--git URL` with Nixpacks, Railpack or a Dockerfile ([Builds](builds.md)). |
| Database | A [database](databases.md): Postgres, MySQL, MariaDB, MongoDB or Redis, with generated credentials and S3 [backups](databases.md#backups). |
| One-click service | A [template](templates.md): isb's built-in ones, or Coolify's own catalog, translated as it is fetched ([Coolify templates](templates.md#coolify-templates)). |
| Docker Compose resource | A [stack](../concepts/stacks.md) in isb's compose format, or a template. |
| Environment variables, shared variables | An app's `.env` editor; secrets are org [secrets](secrets.md). Shared variables are per app only: put the value in a secret and reference it. |
| Magic variables (`SERVICE_URL_*`, `SERVICE_PASSWORD_*`) | Template variables: domains and generated secrets, made when a template is deployed. |
| Persistent storage | Named volumes ([Volumes](volumes.md)). An app cannot bind-mount host paths. |
| Domains, the proxy | `domains:` on an app, certificates from Let's Encrypt through Caddy, `host: auto` for a generated name, or an org's Cloudflare Tunnel ([Domains and ingress](domains.md)). |
| Webhooks, preview deployments | A signed webhook per app ([Webhooks](deploy-apps.md#webhooks)) and [Previews](previews.md) per pull request. |
| Scheduled tasks | [Jobs](jobs.md) against an app. |
| Notifications | [Channels](notifications.md): webhook, Slack, Discord, Telegram and email. |

## Coolify's templates

Add Coolify's catalog once (platform admins) and every org can deploy from
it:

```sh
isb template catalog add coolify --format coolify https://github.com/coollabsio/coolify
isb template show coolify/umami           # clean, notes or refused, with the reasons
isb --org acme template deploy coolify/umami --project web
```

A template's `SERVICE_URL_*` becomes a domain (a generated one unless you give
`-s domain_umami=stats.example.com`), `SERVICE_PASSWORD_*` and the other
generated values become secrets, `${VAR:-default}` an input with that default,
and a file with inline `content:` a file in the app. About two in three of
Coolify's templates deploy with nothing to note. What does not: anything that
asks for the Docker socket, host paths, added capabilities, host networking
or privileged mode, a volume shared by several services, or a one-shot job
(a migration that must finish first) is refused with the reason; the
numbers and the full mapping are in [Coolify templates](templates.md#coolify-templates).

## Bringing your apps over

There is no import of Coolify's database. Recreate each resource: an
application with `isb app create` and its environment with `isb app env-set`;
a database with `isb db create`, loading a dump taken with the engine's own
tools ([Coming from Dokploy](from-dokploy.md#bringing-your-apps-over) walks
through it); a one-click service by deploying its template, then restoring
its data into the new volumes. Move domains last: add them to the apps,
deploy, and switch DNS once `isb stack ps` shows the certificate issued.
