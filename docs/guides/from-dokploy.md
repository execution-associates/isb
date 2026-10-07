---
title: Coming from Dokploy
description: How Dokploy's projects, applications, databases, templates and integrations map onto isb, and what works differently.
order: 5
---

If you run apps on Dokploy, most of what you do there has a direct
equivalent in isb, and the web UI follows Dokploy's layout on purpose:
projects with environments, an app's General, Environment, Domains,
Deployments, Logs and Monitoring tabs, one-click templates. What changes is
underneath. Dokploy runs every service on one shared Docker daemon; isb runs
each org in its own restricted incus project, with unprivileged system
containers (or VMs), its own network and quotas, so tenants are really
apart ([Security model](../concepts/security.md)). And agents are
first-class operators, through each org's MCP server
([Agents and MCP](agents.md)).

## The model

| Dokploy | isb |
|---|---|
| Organization | An [org](../concepts/orgs.md): an incus project with its own bridge, network ACL and quotas. |
| Project, environment (default `production`) | The same: a [project](deploy-apps.md#projects-and-environments) with environments, `production` first. Each environment runs as one stack, `<project>-<env>`. |
| Application (git or image source) | An [app](deploy-apps.md): `--image REF` or `--git URL` with a builder. Zip uploads are not a source. |
| Compose service | A [stack](../concepts/stacks.md): a compose file in isb's format (`isb stack deploy`, or the `stack_deploy` tool). |
| Database service | A [database](databases.md): Postgres, MySQL, MariaDB, MongoDB or Redis, with generated credentials. libSQL is not offered. |
| Template | A [template](templates.md); Dokploy's own catalog can be added and is translated strictly. |
| Docker Swarm on one host | The `isb serve` daemon: replicas, health checks, a load balancer, rolling updates and rollbacks ([Stacks](../concepts/stacks.md)). |
| Remote servers over SSH | One isb per host. Install isb on each and connect each host's MCP server to the same agent ([Several hosts](agents.md#several-hosts)). |

## Day to day

| In Dokploy | In isb |
|---|---|
| Environment tab (`.env` editor) | `isb app env` / `env-set`, or the Environment tab. Secrets are org secrets referenced as `${{secret.NAME}}`; values are never shown back ([Deploy apps](deploy-apps.md#the-environment-editor)). |
| Shared project or environment variables | Per app only. Share a value by putting it in an org secret and referencing it from each app. |
| Deploy button, deployment history, rollback | `isb app deploy`, `deployments`, `rollback` (last 30 kept, image pinned by digest; a rollback reuses the old image and settings without building). |
| Deploy webhook | One per app, `POST /api/v1/webhooks/<org>/<app>`, signed for GitHub, GitLab, Gitea and Forgejo, or `?token=` ([Webhooks](deploy-apps.md#webhooks)). Bitbucket and GitHub Apps are not supported. |
| Preview deployments | [Previews](previews.md) per pull request, isolated from production, with fork pull requests off by default. |
| Builders: Nixpacks, Railpack, Dockerfile | The same three, in a throwaway sandbox (a VM by default for git apps) ([Builds](builds.md)). Heroku and Paketo buildpacks and the static builder are not offered; Railpack builds static sites. |
| Registries | A local registry per host, one repository per org. External registries are pulled from (`docker:`, `ghcr:`, `quay:`, `oci:`) but not pushed to. |
| Domains, Let's Encrypt, sslip.io names | `domains:` on an app, certificates from Let's Encrypt through Caddy, `host: auto` for a generated sslip.io name, or the org's own Cloudflare Tunnel ([Domains and ingress](domains.md)). Custom certificates, basic auth and regex redirects are not supported. |
| Mounts: volume, file | Named volumes (`NAME:/path`) and `files` (an org secret's value as a file). Apps cannot bind-mount host paths. |
| Database backups to S3 | [Backups](databases.md#backups): the engine's own dump streamed to any S3-compatible bucket, on a cron schedule, with `keep`. Restores go into the same database or a new one beside it. |
| Volume backups | [Volume snapshots, backups and staged restores](volumes.md), restored beside the live volume. |
| Schedules | [Jobs](jobs.md) in a running replica or a one-off instance of the app's image. Commands on the host are not a job target. |
| Notifications | [Channels](notifications.md): webhook, Slack, Discord, Telegram and email. Other providers can be reached through a webhook. |
| Monitoring | Per-replica CPU, memory, network and disk history, 30 days ([Metrics](../operations/metrics.md)), and the Monitoring tab. |
| Logs, web terminal | The Logs tab (`isb stack logs`) and a terminal into any replica; plus [SSH](ssh.md) into an org's instances. |
| Users: email and password, GitHub, Google, passkeys, invitations, API keys | The same, plus any OpenID Connect provider ([Sign-in](sign-in.md)). API tokens are `Authorization: Bearer isb_tok_...`, optionally narrowed by scope. TOTP two-factor is not built in; put SSO or Cloudflare Access in front. |
| Roles owner, admin, member | `owner`, `admin`, `member` and a read-only `viewer`, per org ([Users, roles and superadmins](../concepts/access.md)). Grants are per org, not per project. |
| Command palette | ⌘K / Ctrl+K in the web UI. |

## Bringing your apps over

There is no import of Dokploy's database. Recreate each service in isb:

1. Create the org, then a project with the same environments
   (`isb project create shop --env production --env staging`).
2. For each application, `isb app create` with the same image or repository
   and builder, then paste its `.env` into `isb app env-set`, moving any
   secret values into org secrets first (`isb secret create`). Point the
   repository's webhook at the app's new webhook URL.
3. For each database, `isb db create` with the same engine, then load a dump
   taken on Dokploy with the engine's own tools: for example, publish the new
   database on loopback (`isb db create pg --project shop --engine postgres
   --publish 127.0.0.1:15432`), read its password with `isb db show pg
   --show-password`, and run `pg_restore` against `127.0.0.1:15432`.
   `isb backup restore` reads only objects isb's own backups wrote
   ([Databases](databases.md#restores)), so set up an isb backup schedule
   once the data is in.
4. For a template, deploy isb's built-in one if there is one, or add Dokploy's
   catalog and deploy the same template from it:

   ```sh
   isb template catalog add dokploy --format dokploy https://templates.dokploy.com
   isb template show dokploy/ntfy            # compatibility: clean, notes or refused, with reasons
   isb --org acme template deploy dokploy/ntfy --project tools
   ```

   A Dokploy template that asks for something isb will not grant (privileged
   mode, host paths, the Docker socket, host networking) is refused with the
   reason ([Dokploy templates](templates.md#dokploy-templates)).
5. Move the domains last: add them to the apps, deploy, and switch DNS once
   `isb stack ps` shows the certificate issued.
