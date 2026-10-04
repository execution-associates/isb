# Dokploy, as a requirements reference

What Dokploy (Apache-2.0 parts only; nothing under `proprietary/` was read)
does, condensed from a read of its source on 2026-10-03 (HEAD 48504fde). A
reference for isb's platform work (TASKS.md), not a spec: isb's ontology is
different (orgs are incus projects; services are incus instances).

## Model
- Hierarchy: organization → project (shared env) → environment (default
  "production", shared env) → service. Services: application (git/image/zip
  source + build), compose (raw or git compose file), databases (postgres:18,
  mysql:8, mariadb:11, mongo:8, redis:8, libsql).
- Service attachments: domains (host, path, internalPath, stripPath, port,
  https, certificateType letsencrypt|none|custom, middlewares), mounts
  (bind | volume | file with inline content), ports (published/target,
  tcp/udp), redirects (regex; www presets), basic auth users, repo patches.
- Deploy history: deployment rows (status, log file, commit title), keep last
  10; rollbacks = tagged image + full config snapshot; preview deployments.
- Org integrations: registries, git providers (GitHub App, GitLab, Gitea,
  Bitbucket), SSH keys, certificates, S3 destinations, backups (cron, keep N),
  volume backups, schedules (cron: in container / host / panel, timezone),
  notifications (Slack, Telegram, Discord, SMTP, Resend, Gotify, ntfy,
  Mattermost, Pushover, webhook, Lark, Teams; events: deploy ok, build error,
  db backup, volume backup, restart, cleanup, server threshold), remote
  servers, DNS providers, vault providers, AI provider.

## Auth (better-auth)
- Email/password, GitHub + Google OAuth, passkeys, TOTP 2FA, API keys
  (`x-api-key`, bound to user + org), organizations, invitations by link;
  first visitor becomes owner, later sign-ups only by invitation.
- Roles owner/admin/member; members get per-project/env/service grants.
  (SSO/OIDC/SAML management, SCIM, custom roles, audit log: proprietary.)

## Deploy flow
- Triggers: UI/API; generic webhook `/api/deploy/<token>` (GitHub/GitLab/
  Gitea/Bitbucket push, Docker Hub/GHCR image events; branch + watchPaths
  filters); GitHub App webhook (signature, tags vs push, PR previews).
- Per-server FIFO queue with concurrency; cancel/kill; logs to files,
  tailed over websocket; deployment title = commit message.
- Builders: nixpacks, railpack (BuildKit frontend, env as --secret), heroku
  and paketo buildpacks, Dockerfile (context, target, args, secrets), static
  (nginx, SPA mode). Optional registry push; rollback tags `app:vN`.
- Runs as a swarm service; default update start-first, failure rollback.

## Ingress
- Traefik (80/443, HTTP-01 ACME), one dynamic file per app: routers per
  domain, https redirect, strip/add prefix, redirect regex, basic auth.
- Generated domains: `<app>-<hex6>-<ip-dashed>.sslip.io`; DNS validation
  helper that detects CDNs. Custom certificates. Panel domain setting.

## Databases and backups
- Generated credentials, data volume, internal host = service name,
  optional external port, start/stop/rebuild/change password.
- Backups: dump via exec (pg_dump -Fc, mysqldump --single-transaction,
  mariadb-dump, mongodump) | gzip | rclone to S3, keep N; restore from list.

## Env
- project/environment/service env, encrypted at rest; interpolation
  `${{project.X}}`, `${{environment.X}}`, `${{X}}` only when referenced;
  `${{vault.…}}` from external secret managers; build env/args/secrets.

## Remote servers
- SSH key + setup script (installs docker, swarm, traefik, builders), then
  everything over ssh2 / docker-over-ssh; validate + security audit; build
  servers; cluster workers; service transfer between servers.

## Observability
- Go metrics agent per server (host + per-container, retention, thresholds
  → notifications); live logs (tail/since/search); web terminal (exec with
  shell picker; host shell over ssh); access-log analytics page.

## Templates
- `templates.dokploy.com/meta.json`; each = `docker-compose.yml` +
  `template.toml` ([variables] with ${domain} ${password:N} ${base64:N}
  ${uuid} ${jwt:…}; [config] domains, env, mounts; isolated flag).

## UI
- Sidebar: Home, Projects, Overview, Deployments, Monitoring, Schedules,
  Traefik files, Docker, Swarm, Networks, Requests, Settings.
- Environment canvas with "Create service" (App, Compose, Database,
  Template, Import), bulk actions.
- Application tabs: General, Environment, Domains, Deployments, Preview
  Deployments, Schedules, Volume Backups, Logs, Patches, Monitoring,
  Advanced. Databases: General (internal + external URLs), Environment,
  Logs, Monitoring, Backups, Advanced.
- Polish: onboarding wizard ending in a sample deploy; first-run owner
  setup with auto-detected IP; sslip.io URL without DNS; "test connection"
  everywhere; copyable webhook URL; live logs drawer; empty states;
  permission-aware messages; command palette.

## Gaps isb fills by design
- Real tenant isolation (incus projects, unprivileged, per-instance uid
  ranges) instead of one shared Docker daemon.
- Agents as first-class operators (per-org MCP), sandboxes beside apps.
