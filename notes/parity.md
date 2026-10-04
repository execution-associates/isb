# API parity

The web UI and agents reach `isb serve` through the same
[tools](../docs/reference/mcp-tools.md): a page that deploys an app calls `app_deploy`, as an
agent does. This page lists every capability with where a person finds it
in the web UI and which tool an agent calls, and says why when one side
deliberately has no equivalent. It is checked: the web UI's parity test
(`web/src/lib/parity.test.ts`) fails when a tool is missing here, when the
UI calls a tool or endpoint that does not exist, or when an identity
endpoint has neither a tool nor a documented reason in the
[OpenAPI document](../docs/reference/http-api.md#the-openapi-document) (`x-isb-tool`,
`x-isb-browser-only`).

## Summary

| Category | Count |
|---|---|
| Tools in the web UI and MCP | 149 |
| Account tools, the web UI through the identity endpoints | 20 |
| Tools for MCP and the CLI only | 27 |
| Identity endpoints with a tool | 21 |
| Identity endpoints for the browser only | 23 |
| Other routes with no tool | 4 |

"In the web UI" includes a handful of tools whose data a page shows
through a sibling tool (`environment_list` through `project_list`,
`job_get` through `job_list`, and the like); the tables say which. Every
row of the tables below names its tools, and the MCP/CLI-only and
browser-only rows say why.

## Tools

**Web UI** names the page (and the control on it); *MCP/CLI only* says why
a person on the web does not need it.

### Stacks and sandboxes

| Capability | Web UI | MCP |
|---|---|---|
| List stacks, their health and replicas | App health on every app page; the dashboard | `stack_list` |
| One stack in detail | App page (domains, replicas) | `stack_status` |
| Scale a service | App page, General: Replicas; Stop/Start | `stack_scale` |
| Logs | App page, Logs | `stack_logs` |
| Compose stacks: list, show the file, check it, deploy it, remove it | Projects, Compose stacks; New compose stack (paste YAML); a stack's page: Compose (editor, Changes, review, Deploy), Services, Logs, Remove | `stack_export`, `stack_validate`, `stack_deploy`, `stack_remove` |
| Redeploy one service, roll a stack back, the raw stored definition | *MCP/CLI only*: a service is rolled by changing its file (the stack's Compose tab), and the stored definition is what `stack_export` shows as YAML; the web UI manages apps, which are stacks underneath, with `app_deploy`, `app_rollback` and `app_delete` | `stack_redeploy`, `stack_rollback`, `stack_config` |
| List sandboxes | Workspace, Sandboxes | `sandbox_list` |
| Extend or remove a sandbox | Workspace, Sandboxes: Extend by, Delete | `sandbox_extend`, `sandbox_remove` |
| Create a sandbox, run a command in it | *MCP/CLI only*: sandboxes are agents' scratch machines; a person opens a shell in one from Workspace, Sandboxes, Shell (the terminal websocket) | `sandbox_create`, `sandbox_exec` |
| Sandbox defaults (lifetime, idle limit) | Workspace, Sandboxes: Defaults (admins) | `workspace_settings` |

### Instances: look inside and act on what runs

The tools behind [isb for kubectl users](../docs/guides/kubectl.md): an agent
inspects and drives an org's running instances the way `kubectl` does a
cluster's pods. A person has the app page.

| Capability | Web UI | MCP |
|---|---|---|
| List the org's instances, describe one | *MCP/CLI only*: the app page shows its replicas (General: Scale) with their health, and Monitoring their CPU and memory | `instance_list`, `instance_get` |
| Run a command in an app's replica or any instance | *MCP/CLI only*: a person opens a shell: App, Terminal (the terminal websocket) | `app_exec`, `instance_exec` |
| An app's logs by app name, its resource use, its events | *MCP/CLI only*: the Logs tab calls `stack_logs`, Monitoring reads `metrics_query`, and the activity feed shows the events | `app_logs`, `app_top`, `app_events` |
| Restart an app's replicas, rolling | *MCP/CLI only*: a person deploys, or restarts one replica at a time | `app_restart` |
| Scale an app by its name | *MCP/CLI only*: the Scale control saves the setting and calls `stack_scale` | `app_scale` |
| Replace one replica | App, General: Scale, a replica's Restart | `instance_restart` |
| Copy a small file into or out of an instance | *MCP/CLI only*: the terminal and `isb cp` serve the same need | `instance_file_read`, `instance_file_write` |

### Workspaces

| Capability | Web UI | MCP |
|---|---|---|
| The workspace: status, resources, home, sessions, connect details | Workspace (every tab) | `workspace_get` |
| Build a workspace image from a recipe (the default image or your own) | Workspace create form: Build the default image (platform admins) | `workspace_image_build` |
| Follow an image build's log | Workspace create form, the build log | `workspace_image_logs` |
| List the workspace images isb built | Workspace create form, the image picker | `workspace_image_list` |
| Remove a workspace image isb built | Workspace create form (platform admins) | `workspace_image_remove` |
| Run the workspace's first-boot setup script again | Workspace page | `workspace_setup_run` |
| The workspace's terminal sessions (herdr or plain shells) | Workspace, Terminal | `workspace_terminals` |
| Rename or end a terminal session | Workspace, Terminal: rename a tab, close it (Detach or End) | `workspace_terminal_update` |
| Create, change, delete it | Workspace: Create; Resources, Home, Environment tabs; Delete | `workspace_create`, `workspace_update`, `workspace_delete` |
| Start, stop, restart, rebuild | Workspace header actions | `workspace_start`, `workspace_stop`, `workspace_restart`, `workspace_rebuild` |
| Rotate its token | Workspace, Connect: Rotate token | `workspace_token_rotate` |
| Publish, list and remove ports; open a port's preview | Workspace, Ports: Publish, the bin, Open | `workspace_port_add`, `workspace_port_list`, `workspace_port_remove`, `workspace_port_open` |
| Docker in the workspace (superadmins) | Org settings, Docker in the workspace; the Nesting allowed badge | `org_nesting` |
| List an org's workspaces | *MCP/CLI only*: an org has one workspace unless a platform admin raised `max_workspaces`, and the page shows it | `workspace_list` |
| SSH host keys for `known_hosts` | *MCP/CLI only*: `isb ssh config` pins them for the SSH client; a browser has no use for them | `ssh_host_keys` |

### Secrets

| Capability | Web UI | MCP |
|---|---|---|
| List, create, set, read, delete, refresh | Org, Secrets | `secret_list`, `secret_create`, `secret_set`, `secret_get`, `secret_delete`, `secret_refresh` |
| Inspect a secret's versions and bindings | *MCP/CLI only*: the Secrets page lists what a person needs (driver, version, used by); the raw view is for debugging | `secret_inspect` |
| Break-glass recipients, re-encrypt | *MCP/CLI only*: operator work on the host's age key and `secrets.toml` | `secret_recipients`, `secret_reencrypt` |
| Resolve a compose file's secrets | *MCP/CLI only*: used by `isb stack deploy` while deploying a compose file | `secret_resolve` |

### Projects, apps and previews

| Capability | Web UI | MCP |
|---|---|---|
| Projects and environments | Projects; project page: New environment, Delete | `project_list`, `project_create`, `project_delete`, `environment_create`, `environment_delete` |
| List a project's environments | Shown on the project page (from `project_list`) | `environment_list` |
| Apps: list, show, create, change, delete | Projects, app pages, New app, General, Advanced | `app_list`, `app_get`, `app_create`, `app_update`, `app_delete` |
| An app as a YAML document: show it, check it, diff it, create or replace it (kubectl apply) | App, YAML (editor, Changes, review, Save, Save and deploy) | `app_export`, `app_apply` |
| Environment variables | App, Environment | `app_env_get`, `app_env_set` |
| Deploy, roll back, deployments and their logs | App: Deploy, Deployments, a deployment's page | `app_deploy`, `app_rollback`, `app_deployments`, `app_deployment_log` |
| Webhook URL and secret, deploy key | App, General: Git | `app_webhook`, `app_deploy_key` |
| Previews: list, show, log, redeploy, delete | App, Previews | `preview_list`, `preview_get`, `preview_log`, `preview_redeploy`, `preview_delete` |

### Templates

| Capability | Web UI | MCP |
|---|---|---|
| Browse and deploy one-click apps | Templates; a template's page: Deploy | `template_list`, `template_get`, `template_deploy` |
| Deployed templates | Templates: Deployed | `template_instance_list`, `template_instance_delete` |
| Catalogs | Templates: Catalogs (platform admins) | `template_catalog_list`, `template_catalog_add`, `template_catalog_remove` |

### Databases, backups and volumes

| Capability | Web UI | MCP |
|---|---|---|
| Databases | New database; app page, Database tab | `database_list`, `database_get`, `database_create` |
| Backup destinations | Backups: Destinations | `backup_destination_list`, `backup_destination_create`, `backup_destination_delete`, `backup_destination_test` |
| Backups: schedule, change, run, restore, runs and logs | Database and volume Backups tabs; Backups page | `backup_list`, `backup_create`, `backup_update`, `backup_delete`, `backup_run`, `backup_restore`, `backup_runs`, `backup_run_log` |
| Volumes | Volumes; a volume's page | `volume_list`, `volume_get` |
| Snapshots: take, schedule, delete, runs and logs | A volume's page | `volume_snapshot_create`, `volume_snapshot_schedule`, `volume_snapshot_delete`, `volume_snapshot_runs`, `volume_snapshot_run_log` |
| List snapshots, list staged restores | Shown on a volume's page (from `volume_get`) | `volume_snapshot_list`, `volume_restore_list` |
| Restore (staged), discard a staged restore | A volume's page: Restore; Restores | `volume_restore`, `volume_restore_discard` |

### Jobs

| Capability | Web UI | MCP |
|---|---|---|
| Scheduled jobs: list, create, change, delete, run, runs and logs | App, Jobs | `job_list`, `job_create`, `job_update`, `job_delete`, `job_run`, `job_runs`, `job_run_log` |
| One job | Shown in the Jobs list (from `job_list`) | `job_get` |

### Builds and the registry

| Capability | Web UI | MCP |
|---|---|---|
| Build an image, follow it, list builds | *MCP/CLI only*: a person's builds happen inside an app's deploy (Deployments shows the build log); building an image without deploying it is a pipeline step | `build_run`, `build_logs`, `build_list` |
| The org's images in the local registry, retention | *MCP/CLI only*: images are an implementation detail of deploys (`registry:APP:TAG`); retention is operator work | `registry_list`, `registry_gc` |

### Notifications, uptime and metrics

| Capability | Web UI | MCP |
|---|---|---|
| Channels: list, create, change, delete, test; deliveries | Notifications | `notification_channel_list`, `notification_channel_create`, `notification_channel_update`, `notification_channel_delete`, `notification_test`, `notification_deliveries` |
| One channel | Shown in the Notifications list | `notification_channel_get` |
| Platform notification settings | Notifications (platform admins) | `notification_settings` |
| Uptime monitors: list, create, change, pause, resume, delete | Uptime; each app's Monitoring tab | `monitor_list`, `monitor_create`, `monitor_update`, `monitor_pause`, `monitor_resume`, `monitor_delete` |
| One monitor and its history | Uptime, a monitor's page | `monitor_get`, `monitor_checks` |
| Apps' own monitors | Uptime, Apps' own monitors | `monitor_settings` |
| Metrics history | App, Monitoring | `metrics_query` |

### Orgs, the dashboard and servers

| Capability | Web UI | MCP |
|---|---|---|
| Orgs: show, list, create, change, delete | Org settings; Admin, Orgs | `org_get`, `org_list`, `org_create`, `org_update`, `org_delete` |
| Dashboard and events | Org overview; every app page (live, over the event stream) | `overview`, `events` |
| Ingress and domains | App, Domains | `ingress_status` |
| Server status | Admin, Servers | `server_status` |
| Servers: list, add, follow, remove | Admin, Servers | `server_list`, `server_add`, `server_provision_get`, `server_remove` |
| Rotate a server's certificate | Admin, Servers, a server: Rotate certificate | `server_rotate_cert` |
| Upgrade a server's agent | Admin, Servers, a server: Upgrade (shown when its build differs) | `server_upgrade` |
| One server | The server sheet on Admin, Servers (from `server_list`) | `server_show` |

### Audit, history and the host

| Capability | Web UI | MCP |
|---|---|---|
| Audit log | Org, History (Audit log source) | `audit_list` |
| Verify the hash chains | Admin, History: Verify chain | `audit_verify` |
| History | Org, History; Admin, History | `history_query` |
| Host inventory and policy | Host (superadmins) | `host_inventory`, `host_policy` |
| Superadmin tokens: list, revoke | Host, Tokens | `superadmin_token_list`, `superadmin_token_revoke` |

### Accounts

The web UI's account pages call the [identity endpoints](../docs/reference/identity-api.md);
agents call these tools, which run the same code with the same rules.

| Capability | Web UI | MCP |
|---|---|---|
| Who am I | Every page (the session) | `whoami` |
| Members: list, change a role, remove | Org, Members | `member_list`, `member_update`, `member_remove` |
| Invitations: list, invite, revoke | Org, Members: Invite; Pending | `invitation_list`, `invitation_create`, `invitation_revoke` |
| Agent identities: list, map a tailnet login or tag or an Access email or service token to a role, remove | Org, Settings: Agent identities; Org, MCP: the Tailnet and Access identity cards show them | `agent_identity_list`, `agent_identity_set`, `agent_identity_remove` |
| API tokens: list, create, revoke; every token in an org | Account, API tokens; Org, Agents | `token_list`, `token_create`, `token_revoke` |
| SSH keys | Account, SSH keys | `ssh_key_list`, `ssh_key_add`, `ssh_key_remove` |
| Sessions | Account, Sessions | `session_list`, `session_revoke` |
| Users and platform admins | Admin, Users | `user_list`, `user_update` |

## The identity endpoints

Every `/api/v1/auth/*` endpoint and its tool is in the [table
above](#accounts) and in the OpenAPI document (`x-isb-tool`). These have
no tool, on purpose:

| Endpoints | Web UI | Why no tool |
|---|---|---|
| `POST login`, `POST logout`, `GET`/`POST edge`, `GET providers`, `GET`/`POST oauth/{provider}/start`, `GET oauth/{provider}/callback`, `POST passkeys/login/options`, `POST passkeys/login/verify` | Sign in | A browser sign-in flow: it sets the session cookie a person's browser carries. An agent signs in with a token instead. |
| `GET`/`POST setup` | First-run setup | Happens once, in a browser, as the tailnet or Access identity, or with the setup link from the host. |
| `POST invitations/inspect`, `POST invitations/accept`, `POST password-reset/request`, `POST password-reset/confirm` | Invitation and password reset pages | For someone who holds no credential yet: the token or address they bring is the credential. |
| `POST password`, `GET identities`, `DELETE identities/{id}`, `GET passkeys`, `DELETE passkeys/{id}`, `POST passkeys/register/options`, `POST passkeys/register/verify` | Account, Sign-in methods | A way into the account: changed from a signed-in browser session only, so a leaked token cannot lock its owner out or let an attacker in. |

And these rules hold on both sides:

- **Superadmin tokens** are minted on the host only (`isb token create NAME
  --superadmin`), never over HTTP or MCP, so a stolen HTTP credential, a
  superadmin's included, cannot mint a durable one.
- **A token cannot mint tokens.** `token_create` and `POST tokens` refuse
  API, workspace and superadmin tokens; new tokens come from a browser
  session, an Access or tailnet identity, or the host CLI. A token that
  could mint another would survive its own revocation through the copy, and
  narrowing the copy's scopes or expiry would not change that.
- **A workspace token** reaches no account endpoint and no account tool but
  `whoami`.
- **A token scoped short of `admin`** reads accounts but changes nothing.

## The rest of the HTTP surface

| Route | Web UI | MCP |
|---|---|---|
| `POST /mcp`, `POST /orgs/{org}/mcp`, `POST /orgs/{org}/api/v1/tools/{tool}`, `GET /api/v1/tools` | Org, Agents lists the tools | MCP itself |
| `GET /api/v1/events` | Live updates on app pages | `events` (polled) |
| `GET /api/v1/audit/stream` | History: Live | `audit_list` with `after` |
| `GET /api/v1/history/stream` | History: Live | `history_query` |
| `/orgs/{org}/api/v1/workspace[/ACTION]` | The Workspace page calls the tools directly | The `workspace_*` tools |
| `GET /orgs/{org}/api/v1/ssh` | Workspace, Connect explains `isb ssh-proxy` | `isb ssh-proxy`, the CLI's SSH transport |
| `GET /orgs/{org}/api/v1/terminal` | App, Terminal; Workspace, Terminal | *Browser only*: an interactive terminal for a person; agents run commands with `sandbox_exec`, or SSH |
| `POST /api/v1/webhooks/{org}/{app}` | App, General shows the URL | *Neither*: called by a git host; `app_deploy` and `preview_redeploy` do the same by hand |
| `GET /api/v1/templates/{catalog}/{id}/logo` | Template gallery | *Browser only*: an image; `template_get` names it |
| `GET /healthz` | None | *Neither*: for load balancers and monitors; `overview` is the signed-in view |
