---
title: MCP tools
description: Every tool isb serve offers to agents, people, the web UI and the CLI, grouped by area, with who may call each.
order: 3
---

Everything `isb serve` does is a **tool**: a named operation with a JSON
Schema for its arguments. The same tools are served as MCP (for agents), as
REST (`POST /api/v1/tools/<tool>`), and over the daemon's unix socket (for
the `isb` CLI), and the web UI is built on them. One authorizer judges every
call, whatever the door. This page lists all of them; the
[HTTP API](http-api.md) says how to call them, and each area links to its
guide.

The list a caller actually sees can be shorter: `--allow-tools` and
`--deny-tools` hide tools from remote callers
([Configuration](configuration.md#daemon-flags)), and a tool hidden that way
does not exist for them. `GET /api/v1/tools` and MCP's `tools/list` return
what this listener offers, with each tool's schema and annotations.

## Who may call what

Every tool takes `org` (default `default`) and works in that org; an
org-bound endpoint (`/orgs/<org>/mcp`, `/orgs/<org>/api/v1/tools/...`)
fills it in and refuses any other value. Then, in order:

| Caller | May call |
|---|---|
| the unix socket, a [superadmin](../concepts/access.md#superadmins) | every tool, in every org |
| a platform admin | every tool but the superadmin ones, in every org |
| an org **owner** or **admin** | every tool of the org; plus managing members and tokens, reading the audit log |
| an org **member** | every tool of the org, except where the table says admins |
| an org **viewer** | read-only tools only, and none that hands out secret material |
| anyone else | nothing (a token or session for another org gets "no access to org") |

- **Read-only** means the tool is annotated `readOnlyHint` and is not a
  secret read. `secret_get`, `secret_resolve` and `app_webhook` are secret
  reads, and so is `database_get` with `reveal: true`; viewers and `read` or
  `deploy` tokens are refused them, and every call is in the audit log.
- **Platform tools** reach across orgs and are for platform admins only,
  whatever the caller's role in an org (an org owner's token is refused):
  `org_list`, `org_create`, `org_update`, `org_delete`, every `server_*`
  tool, `server_status`, `registry_gc`, `notification_settings`,
  `template_catalog_add`, `template_catalog_remove`, `audit_verify`,
  `user_list` and `user_update`. `secret_reencrypt` with `all: true` too.
- **Cross-org reads** (`overview`, `events`, `stack_list`,
  `ingress_status`) are open to anyone signed in and show only the caller's
  orgs. `audit_list` and `history_query` filter themselves the same way.
- **Superadmin tools** (`host_inventory`, `host_policy`,
  `superadmin_token_list`, `superadmin_token_revoke`, `org_nesting`) are
  refused to everyone else, platform admins included.
- **API token scopes** narrow a token below its role: `read` (read-only
  tools), `deploy` (`read` plus `stack_deploy`, `stack_redeploy`,
  `stack_rollback`, `stack_scale`, `app_deploy`, `app_rollback`,
  `build_run`), `admin` (the whole role) and `tool:GLOB`. See
  [Scopes](../concepts/access.md#scopes).
- **Account tools** ([below](#accounts)) are judged by the identity
  endpoints' own rules rather than a role in `org`: a workspace token reaches
  none of them but `whoami`, a token scoped short of `admin` only reads them,
  and a token never mints a token.
- Some tools check more themselves; the **Who** column says so.

In the tables, **Who** is the least role that may call the tool: *viewer*
means every member of the org, *member* means members, admins and owners.

## Stacks

[Stacks](../concepts/stacks.md).

| Tool | Who | Does |
|---|---|---|
| `stack_deploy` | member | Deploy or update a stack from compose YAML (`name`, `compose`, `vars`, `secrets`, `base_dir`, `wait`, `timeout`, `dry_run`). Returns the change per service; `wait` blocks until it settles; `dry_run` returns the changes without deploying. |
| `stack_list` | anyone signed in | Every stack in the caller's orgs with its services' replica, health and rollout state. |
| `stack_status` | viewer | One stack in detail: per service its revision, state, message, every replica (status, health, IP, in rotation, restarts, last probe output), published ports with their backends, and each domain with its URL and certificate state. |
| `stack_config` | viewer | The deployed compose file, and its secrets as references (store name, driver, version), never values. |
| `stack_logs` | viewer | Recent output of a service's replicas (`slot`, `lines`): the supervised command's journal, or an OCI image's console. |
| `stack_scale` | member | Set a service's replicas (0 stops it without removing it). |
| `stack_redeploy` | member | Replace a service's replicas though nothing changed: a moved tag, changed bind-mounted files. |
| `stack_rollback` | member | Back to the previous deployment; a second rollback undoes the first. |
| `stack_remove` | member | Delete a stack's instances and ports (volumes with `volumes: true`), and the `<stack>_<key>` secrets it stored that no other stack uses. |

## Sandboxes and workspaces

[Workspaces and sandboxes](../concepts/workspaces.md).

| Tool | Who | Does |
|---|---|---|
| `sandbox_create` | member | Create or reconcile one sandbox from a service spec (an object or YAML, `container_name` set), with `expires` and `idle_timeout` (the org's defaults otherwise: 24h and 2h) and `wait_ready`. Remote callers' specs are held to the [remote-spec policy](../concepts/security.md#the-remote-spec-policy), and their sandboxes labelled `isb.owner=<identity>`. `spec.egress` confines the sandbox's network to a list of hosts (or none) and gives it [secrets that never enter the guest](../guides/egress.md); a secret the org does not hold is refused. |
| `sandbox_list` | viewer | Instances, filtered by labels and `kind` (`sandbox`, `workspace`, `replica`, `build`): creator, age, expiry, idle timeout, last activity, limits and use, and `mine`. Remote callers see only instances isb manages. |
| `sandbox_exec` | member | Run argv in a sandbox: exit code, stdout, stderr (each capped at 256 KiB, keeping the end), optional `stdin` text, `user`, `cwd`, `env`, `timeout` (default 10m). |
| `sandbox_extend` | the sandbox's creator, or admin | Push the expiry out (`by`, default 24h, at most 30 days from now) or change `idle_timeout`. |
| `sandbox_remove` | member | Delete a sandbox (not a stack replica, nor the workspace). |
| `ssh_host_keys` | viewer | An instance's SSH host public keys, for pinning, and the user `isb ssh-config` logs in as by default. |
| `workspace_get` | viewer | The workspace (or `null`) and the org's settings: definition, status, resources, home, live sessions, last activity, token metadata, `connect` (`url`, `mcp_url`), sandbox count. |
| `workspace_list` | viewer | Every workspace in the org. |
| `workspace_create` | admin | `image` (default `isb-workspace`, then `dev-base`, where the host has it, else `images:ubuntu/24.04`), `name`, `user`, `cpus`, `memory`, `root_size`, `home_size`, `env`, `secrets`, `labels`, `token_role`, `setup` (a first-boot script); `home_bind` (a host folder as the home) is for superadmins. |
| `workspace_update` | admin | Any of those but `name`, `user`, `home_bind`; resizing needs `confirm: true`. |
| `workspace_start` | member | Start it and deliver its credentials. |
| `workspace_stop`, `workspace_restart` | member | Without `confirm: true`, only report the live sessions it would end. |
| `workspace_rebuild` | admin | A fresh machine from its image (or `image`), same home and token; `confirm`. |
| `workspace_delete` | admin | The machine and its token, and the home unless `keep_home`; `confirm`. |
| `workspace_token_rotate` | admin | A new token, delivered inside; the old one stops at once. |
| `workspace_settings` | member reads, admin changes | `sandbox_expiry`, `sandbox_idle`; `max_workspaces`, `home_kind`, `home_pool` are for platform admins. |
| `workspace_setup_run` | admin | Run the first-boot script again, as root: now when running, else on the next start; outcome and output in the history (`workspace.setup`). |
| `workspace_terminals` | member | The web terminal's mode (`herdr` or `shell`) and its herdr sessions. |
| `workspace_terminal_update` | member | A herdr session: `rename`, or `end: true`. |
| `workspace_image_build` | platform admin | Build a workspace image from a recipe (default: isb's, as `isb-workspace`) in a throwaway container in `isb-system`; returns an `id`. [Workspace images](../guides/workspace-images.md). |
| `workspace_image_logs` | platform admin | A build's state and log lines (`since`, `wait`). |
| `workspace_image_list` | platform admin | The images isb built, recent builds, and whether the default image is current. |
| `workspace_image_remove` | platform admin | Remove an image isb built (never another). |
| `workspace_port_list` | member | The published ports: each with its preview host, and its ingress hostname, URL and state. |
| `workspace_port_add` | member | Publish `port`; with `host` (a hostname, `default` or `auto`) also through the org's ingress. |
| `workspace_port_remove` | member | Stop publishing `port`. |
| `workspace_port_open` | member | A one-time link (60 s) to the port's preview; `origin` when the daemon has no `--preview-domain`. |

## Secrets

[Secrets](../guides/secrets.md). Values travel base64.

| Tool | Who | Does |
|---|---|---|
| `secret_create` | member | Create (`name`, `value`, optional `driver`, `labels`); fails if it exists. |
| `secret_set` | member | A new value and version (creates it in the local store if missing); `rolled` lists the stacks rolling to it. |
| `secret_get` | member | `{meta, value}`. A secret read: always audited. |
| `secret_list` | viewer | Metadata, never values; `used_by` per secret, and `references`: the driver references stacks use. |
| `secret_inspect` | viewer | One secret's metadata. |
| `secret_delete` | member | Refused while a deployed stack uses it. |
| `secret_refresh` | member | Re-read from an external source (a store name, or a stack's driver reference); `rolled` lists the stacks rolling. |
| `secret_reencrypt` | member (platform admins for `all: true`) | Re-encrypt to the current recipients. |
| `secret_recipients` | viewer | The public keys values are encrypted to: the daemon's, then the break-glass ones. |
| `secret_resolve` | local callers only | The values of a compose file's `external`/`age`/`driver` secrets, for `isb up`. |

## Projects, apps and previews

[Deploy apps](../guides/deploy-apps.md), [Preview deployments](../guides/previews.md).

| Tool | Who | Does |
|---|---|---|
| `project_create` | member | A project (`name`, `description`, `environments`, default `production`). |
| `project_list` | viewer | Projects with each environment's stack and apps. |
| `project_delete` | member | A project with no apps. |
| `environment_create`, `environment_list`, `environment_delete` | member (list: viewer) | A project's environments; one with apps cannot be deleted. Each runs its apps as the stack `<project>-<env>`. |
| `app_create` | member | An app: an image or a git source with a builder, plus env, domains, volumes, files, ports, replicas, port, health check, resources, command, user, working directory, previews (`deploy: true` deploys it too). Returns the app and its webhook secret. |
| `app_get`, `app_list` | viewer | Settings, stack, service name, current deployment, webhook path, `ingress_enabled`; env with `{secret: NAME}` references. |
| `app_update` | member | A merge patch of settings (`null` clears one); takes effect at the next deploy (`deploy: true`). The result has a `warning` when the app has domains and the server runs without an ingress. |
| `app_delete` | member | Its service leaves the stack; records, checkout, webhook secret and deploy key go; named volumes are kept. |
| `app_deploy` | member | Queue a deployment (`wait`, `timeout`). |
| `app_rollback` | member | Queue a deployment of an earlier one's image and settings, without building (`deployment`, `wait`). |
| `app_deployments` | viewer | The history, newest first (`limit`). |
| `app_deployment_log` | viewer | A deployment's log from a byte `offset`, with its record. |
| `app_env_get` | viewer | The environment as `.env` text; secret references, never values. |
| `app_env_set` | member | Replace it with `.env` text (`deploy`). |
| `app_webhook` | member | The webhook path and secret (`rotate`). A secret read. |
| `app_deploy_key` | member | A new ed25519 deploy key; returns the public half. |
| `preview_list` | viewer | Previews of one app (`name`) or of every app in the org. |
| `preview_get` | viewer | One preview (`name`, `number`) with its deployments. |
| `preview_log` | viewer | A preview deployment's log from an `offset`. |
| `preview_redeploy` | member | Fetch the head again, build, roll (`wait`). |
| `preview_delete` | member | Remove one now (`wait`). |

## Templates

[Templates](../guides/templates.md).

| Tool | Who | Does |
|---|---|---|
| `template_list` | viewer | The built-in catalog and added ones; filter by `query` words, `tag`, `catalog`. |
| `template_get` | viewer | A template's variables, apps, notes and, for a Dokploy template, its translation report (`compatibility`). |
| `template_deploy` | member | Deploy into a project environment as apps (`template`, `project`, `environment`, `name`, `values`, `dry_run`, `wait`, `timeout`); without `wait`, the first app's deployment comes back as `first_deployment`. |
| `template_instance_list` | viewer | Deployed templates: apps, secret names, non-secret values, URLs. |
| `template_instance_delete` | member | Its apps (named volumes kept) and its `tpl.<name>.*` secrets. |
| `template_catalog_list` | viewer | The added catalogs. |
| `template_catalog_add`, `template_catalog_remove` | platform admin | Add (or replace) or remove a catalog: a host directory or an https URL, `native` or `dokploy` format. |

## Databases and backups

[Databases, backups and restores](../guides/databases.md).

| Tool | Who | Does |
|---|---|---|
| `database_create` | member | A database app (`engine`, `version`, `database`, `user`, `publish`, `env`, `resources`), deployed unless `deploy: false` (`wait`). |
| `database_list` | viewer | Databases with connection details, the password as a secret reference. |
| `database_get` | viewer; member with `reveal` | One database; `reveal: true` adds the password and URL values (a secret read). |
| `backup_destination_create` | member | An S3-compatible bucket; key pairs kept as org secrets; tested unless `test: false`. An endpoint on the daemon's own host (loopback, link-local) is for local callers and platform admins only. |
| `backup_destination_list` | viewer | Destinations, key pairs as secret names. |
| `backup_destination_delete` | member | A destination no backup uses, with the key secrets isb stored for it. |
| `backup_destination_test` | member | Write, `HEAD` and delete a small object. |
| `backup_create`, `backup_update` | member (admin for a `volume`) | A schedule for a `database` or a named `volume`: cron, `timezone`, `keep` (7), `compression` (`gzip`, `zstd`, `none`), `enabled`, `missed_grace`. |
| `backup_delete` | member (admin for a volume) | The schedule and its records; files stay in the bucket. |
| `backup_list` | viewer | Schedules with last and next run; with `name`, the backup's files in the bucket. |
| `backup_run` | member (admin for a volume) | Back up now (`wait`, `timeout`). |
| `backup_runs` | viewer | A backup's runs, or the org's restores with `restores: true`. |
| `backup_run_log` | viewer | A run's log from an `offset` (`restore: true` for a restore run). |
| `backup_restore` | member | Restore a database backup into `target` (needs `confirm: true`) or `new`. Volume backups restore with `volume_restore`. |

## Volumes

[Volumes](../guides/volumes.md). Every change here is for the org's admins
and owners.

| Tool | Who | Does |
|---|---|---|
| `volume_list` | viewer | The org's named volumes: instances using each, snapshot schedule, staged restores. |
| `volume_get` | viewer | One volume: instances, settings and next run, snapshots, staged restores, backups of it. |
| `volume_snapshot_list` | viewer | Snapshots, newest first, with kind (`auto`, `manual`, `other`). |
| `volume_snapshot_create` | admin | Snapshot now (`snapshot` name, `wait`, `timeout`), after the pre-snapshot hook. |
| `volume_snapshot_delete` | admin | Delete a snapshot. |
| `volume_snapshot_schedule` | admin | A merge patch: `schedule` (`null` removes it), `timezone`, `keep`, `enabled`, `missed_grace`, `hook_timeout`, `hook_required`. |
| `volume_snapshot_runs`, `volume_snapshot_run_log` | viewer | Snapshot runs and their logs. |
| `volume_restore` | admin | Restore staged from a `snapshot`, a `backup` (+`key`) or `destination` + `key`, into a new volume at `/restore/<stamp>` (`instance`, `wait`). |
| `volume_restore_list` | viewer | Staged restores. |
| `volume_restore_discard` | admin | Detach and delete one by `stamp`. |

## Jobs

[Scheduled jobs](../guides/jobs.md).

| Tool | Who | Does |
|---|---|---|
| `job_create` | member | `name`, `schedule`, `timezone`, `target` (`{app}` or `{stack, service}`), `mode` (`exec`, `run`), `command`, `timeout`, `concurrency`, `keep`, `enabled`, `user`, `cwd`, `env`, `missed_grace`. |
| `job_list`, `job_get` | viewer | Jobs with their next and last run. |
| `job_update` | member | A merge patch; the name is fixed. |
| `job_delete` | member | The job and its run records (refused while it runs). |
| `job_run` | member | Run now (`wait`, `timeout`). |
| `job_runs`, `job_run_log` | viewer | The history; one run's output from an offset. |

## Builds and the registry

[Builds and the local registry](../guides/builds.md).

| Tool | Who | Does |
|---|---|---|
| `build_run` | member | Build a host directory (`app`, `context`, `subdir`, `builder`, `dockerfile`, `target`, `args`, `tag`, `untrusted`, `timeout`) into the org's registry repository; returns an id. A remote caller's `context` must be under a `--bind-root`. |
| `build_logs` | viewer | A build's state and log lines from `since`, waiting up to 30 s (`wait`) for more; `image` and `digest` when it succeeded. |
| `build_list` | viewer | The org's recent builds. |
| `registry_list` | viewer | The org's images: apps, tags, digests, push times. |
| `registry_gc` | platform admin | Keep the newest `keep` tags per app (10) and whatever deployed stacks run or would roll back to; `dry_run`. |

## Notifications and metrics

[Notifications](../guides/notifications.md), [Uptime monitoring](../guides/uptime.md), [Metrics](../operations/metrics.md).

| Tool | Who | Does |
|---|---|---|
| `notification_channel_create` | member | `name`, `provider` (webhook, Slack, Discord, Telegram, email; secrets named, never values), `rules`, `enabled`. |
| `notification_channel_list`, `notification_channel_get` | viewer | Channels with their last delivery. |
| `notification_channel_update` | member | Replace `provider`, `rules` or `enabled`; fields left out are kept. |
| `notification_channel_delete` | member | The channel and its delivery log (its secrets stay). |
| `notification_test` | member | Send a test message now; returns the delivery. |
| `notification_deliveries` | viewer | A channel's last 50 deliveries, newest first. |
| `notification_settings` | platform admin | `allow_private_targets`, server-wide. |
| `monitor_create`, `monitor_update` | member | An uptime monitor: `name`, `type` (`http`, `tcp`, `app`), its target, `expected_status`, keywords, `headers` (values or secret names), `interval`, `timeout`, thresholds, `cert_expiry_days` ([Uptime monitoring](../guides/uptime.md)). |
| `monitor_list`, `monitor_get` | viewer | Monitors with status, last check, uptime (24 h, 7 d, 30 d), latency p50/p95, bars; the org's incidents and settings. |
| `monitor_checks` | viewer | A monitor's history over `range`: buckets, uptime, raw checks. |
| `monitor_delete`, `monitor_pause`, `monitor_resume` | member | Remove (with its history), stop or start checking. |
| `monitor_settings` | member | `auto_monitors` (apps with a domain get their own monitor), `exclude_apps`. |
| `metrics_query` | viewer | Metrics history: `metric`, `app` or `stack`/`service` or `instance`, `range` or `from`/`to`, `step`, `aggregate`. |

## Orgs, the ingress and the dashboard

[Orgs](../concepts/orgs.md), [Domains and ingress](../guides/domains.md).

| Tool | Who | Does |
|---|---|---|
| `org_get` | viewer | Limits and per-instance defaults, bridge and subnet, egress exceptions, bind roots, service-name domain, counts, and `placement` (`kind`, `server`, `isolation`). |
| `org_list` | platform admin | Every org, as `org_get` shows one, with the server it runs on. |
| `org_create` | platform admin | `org`, `cpus`, `memory`, `disk`, `instances`, `default_cpus`, `default_memory`, `egress`, `placement` (`"local"`, `{"server": NAME}`, `{"vm": {cpus, memory, disk}}`), `wait`. Bind roots are set on the host only. |
| `org_update` | platform admin | Limits, defaults or `egress` (which replaces the list; `[]` clears it); a different placement is refused. |
| `org_delete` | platform admin | Refused while stacks are deployed; `force` deletes remaining sandboxes; `delete_vm` deletes a dedicated VM. |
| `ingress_status` | anyone signed in | Listeners, CA, the Caddy process, every routed domain (URL, certificate state, upstreams), conflicts and refusals, each tunnel org's cloudflared. The caller's orgs only. |
| `overview` | anyone signed in | Everything a dashboard shows in one call: host CPU and memory with history, every stack in detail, sandboxes with CPU and memory, the latest event number. |
| `events` | anyone signed in | The event feed after a `since` cursor (`limit`), waiting up to 30 s (`wait`) for one. |
| `server_status` | platform admin | isb's and incus' versions, and the balancer's routes with live counters. |

## Audit and history

[The audit log](../operations/audit.md), [The history](../operations/history.md).

| Tool | Who | Does |
|---|---|---|
| `audit_list` | org owner or admin (their org); platform admin (everything) | Entries filtered by `actor`, `action`, `target` (globs), `outcome`, `surface`, `user_id`, `token_id`, `since`/`until`, `platform`; paged with `before`, tailed with `after`. |
| `audit_verify` | platform admin | Walk the audit log's and the history's hash chains. |
| `history_query` | viewer (their orgs); host-level rows: platform admin | Controller events, incus lifecycle events, audit rows (owners and admins) and markers, merged; filter by `object` (`exact`), `kind`, `source`, `actor`, `since`/`until`, `platform`; `correlate` links incus changes to their likely audit row. |

## Servers

[Servers and dedicated VMs](../guides/servers.md). All for platform admins.

| Tool | Does |
|---|---|
| `server_add` | Bootstrap a box over SSH (`name`, `ssh`, `ssh_port`, `key` (a path, local CLI only) or `ssh_key` (the key itself), `address`, `agent_port`, `allow_from`, `isb_binary`, `version`, `self_binary`, `public_ingress`); `wait: false` answers at once. |
| `server_list` | Servers with health, orgs and `version` (build, protocol, skew against this control plane, upgradable, last upgrade); servers being added (`provisions`); `dedicated_vm` (whether this host can run dedicated VMs); `suggested_allow_from`. |
| `server_show` | One server: address, how it was added, certificate fingerprint and expiry, health, orgs. |
| `server_remove` | Forget a server (refused while it holds orgs; a dedicated VM is deleted with it). |
| `server_rotate_cert` | Issue its agent a new certificate. |
| `server_upgrade` | Replace a server's agent (`name`, or `all: true`) with this control plane's build, a release (`version`) or a binary on this host (`isb_binary`, local CLI only); waits for the new build, and the box rolls back if it does not answer. |
| `server_provision_get` | Follow a server (or a dedicated VM, `vm-<org>`) being added: steps, log, state, error. |

A call for an org placed on a server is judged on the control plane, then
again by the server's agent. `events`, `audit_list`, `audit_verify`,
`registry_gc`, `notification_settings`, `server_status`, `template_list`,
`template_get`, the `template_catalog_*` tools, the `server_*` tools and
the [account tools](#accounts) always run on the control plane.

## Accounts

[Users, roles and superadmins](../concepts/access.md). The identity
endpoints' account pages as tools, with the same rules
([Identity API](identity-api.md#endpoints)): a workspace token reaches none
of them but `whoami`; a token scoped short of `admin` only reads them;
nobody outside an org learns about it (`not_found`); only an owner (or a
platform admin) touches an owner or makes one.

| Tool | Who | Does |
|---|---|---|
| `whoami` | anyone signed in, a workspace included | The caller: user, platform admin flag, orgs and roles, how it signed in (`auth`), the orgs it can open, and `superadmin`. |
| `member_list` | viewer | Each member's user (id, email, name), role and last activity. |
| `member_update` | admin | `user_id` or `email`, `role`. |
| `member_remove` | admin; anyone for themselves | `user_id` or `email`. Their account stays. |
| `invitation_list` | admin | Pending invitations. |
| `invitation_create` | admin | `email`, `role` (default member, at most your own). Returns the invitation, its `token` (shown once) and the `link` when `--public-url` is set. isb sends no mail. |
| `invitation_revoke` | admin | `id`. |
| `agent_identity_list` | viewer | The org's tailnet and Access agent identities (kind, subject, role, note) and `available`: the server's tailnet listen addresses and whether Access guards a listener. |
| `agent_identity_set` | admin | `kind` (`tailnet`, `access`), `subject` (a tailnet login or `tag:name`; an Access service token client id or the email of someone who is not an isb user), `role` (`viewer`, `member`, `admin`: never owner, at most your own), `note`. An existing subject changes role. |
| `agent_identity_remove` | admin | `id`. |
| `token_list` | anyone signed in | Your tokens' metadata (only `org`'s when given; an org token sees its org's). `all: true`: every token in `org`, with its holder (admins). |
| `token_create` | a session, or an Access or tailnet identity | `name`, `expires` (`90d`; none: never), `scopes`; confined to `org`. Returns `{token, info}`, the token shown once. **A token cannot mint tokens** (an API, workspace or superadmin token is refused), so revoking a leaked token always ends it. |
| `token_revoke` | its holder; the org's admins | `id`. |
| `ssh_key_list`, `ssh_key_add`, `ssh_key_remove` | anyone with an account | The caller's SSH keys for [isb ssh-proxy](../guides/ssh.md): `public_key` and `name`; `id`. |
| `session_list`, `session_revoke` | anyone with an account | The caller's browser sessions; end one by `id`. |
| `user_list` | platform admin | Every user with their orgs and last activity. |
| `user_update` | platform admin | `user_id` or `email`; `disabled`, `platform_admin`. Not yourself, and never the last enabled platform admin. |

Signing in (passwords, passkeys, providers), sign-up, password resets,
accepting an invitation and changing a way in stay in the browser, and
superadmin tokens are minted on the host only.

## Superadmins

[Users, roles and superadmins](../concepts/access.md#superadmins).

| Tool | Does |
|---|---|
| `host_inventory` | Every incus project and instance on the host, isb's or not: project, org, type, status, addresses, isb's stack and owner labels. |
| `host_policy` | How the daemon serves: listen addresses, Access, the remote tool policy, what remote specs may ask for, and each superadmin source with its allow list and token count. |
| `superadmin_token_list` | Superadmin tokens' metadata, never the token. |
| `superadmin_token_revoke` | Revoke one by `id`. Minting is `isb token create NAME --superadmin`, on the host only. |
| `org_nesting` | Read (`org`) or set (`allow_nesting`) whether the org's workspace may run Docker with `security.nesting` ([The Docker exception](../concepts/security.md#the-docker-exception)). Turning it off is refused while the workspace runs with nesting. |
