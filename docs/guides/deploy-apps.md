---
title: Deploy apps from an image or a git repository
description: Create apps in a project environment, edit their environment, deploy, roll back, and deploy on every push with webhooks.
order: 1
nav_title: Deploy apps
---

An app is the easiest way to run something on `isb serve`: you say what to
run (an image, or a git repository and how to build it) and how (port,
replicas, environment, domains), and isb keeps the deploy history, the logs
and the webhooks. It is the layer Dokploy users expect, built on ordinary
[stacks](../concepts/stacks.md), so everything a stack does (health checks,
rolling updates, the load balancer, service names) works for apps too. The
model (org, project, environment, app) is explained in
[Projects, environments and apps](../concepts/apps.md).

```console
$ isb project create shop                       # environments: production
$ isb app create web --project shop --image docker:traefik/whoami \
    --port 80 -p 127.0.0.1:8080:80 -e GREETING=hello --deploy
created app web: service web.shop-production
webhook: POST /api/v1/webhooks/default/web (secret: `isb app webhook web`)
image docker:traefik/whoami
resolved to docker:traefik/whoami@sha256:c4717a8d...
stack shop-production: web create (rev fc9a8d75, 1 replicas)
service web.shop-production converged
deployment 1 done
$ isb app env web > web.env && $EDITOR web.env && isb app env-set web web.env --deploy
$ isb app deployments web
$ isb app rollback web 1
```

Every command takes `--org ORG`. Over MCP and REST the same operations are
tools ([below](#tools)), so `POST /orgs/<org>/api/v1/tools/app_deploy` with an
API token deploys an app. The web UI's Projects pages use the same tools
([The web UI](../getting-started/web-ui.md)).

## Projects and environments

```text
isb project create NAME [--env E]... [--description D]   environments default to production
isb project ls [--json] | rm NAME | env-add PROJECT ENV | env-rm PROJECT ENV
```

- Project and environment names are lowercase letters, digits and `-`,
  starting with a letter, at most 24 characters each.
- A project starts with one environment, `production`; add `staging`,
  `preview` and so on with `--env` or `env-add`.
- Each environment runs as one stack named `<project>-<env>` (at most 30
  characters), and each app is one service in it. `isb stack ps
  shop-production` shows the apps of project `shop`, environment
  `production`.
- Apps of one environment reach each other by service name,
  `<app>.<project>-<env>` (or `<app>.<project>-<env>.<org>.isb`), as any
  stack's services do ([service discovery](../concepts/stacks.md#service-discovery)).
- A project or environment that still has apps cannot be deleted.
- Environment names ending in `pr-<number>` are refused: that suffix is kept
  for [preview stacks](previews.md).

## An app's settings

```text
isb app create NAME --project P [--environment E] (--image REF | --git URL [--ref R] [--subdir D]
               [--token-secret S | --ssh-key-secret S] [--builder railpack|nixpacks|dockerfile|buildpacks]
               [--dockerfile PATH] [--build-arg K=V]...) [-e K=V]... [--env-from F] [--port N]
               [--replicas N] [-p SPEC]... [-v NAME:/path]... [--domain HOST[/PATH]]...
               [--command CMD] [--cpus N] [--memory M] [--deploy]
isb app ls [--project P] [--json] | show NAME | rm NAME
isb app update NAME [-f PATCH|-] [--image REF] [--ref R] [--replicas N] [--port N] [--deploy]
```

| Field | What |
|---|---|
| `name` | `[a-z0-9-]`, unique in the org; the service name in its stack. Fixed. |
| `project`, `environment` | Where it runs (environment default `production`). Fixed. |
| `source` | `{image: REF}` (`docker:nginx:1.27`, `ghcr:org/app:tag`, a local alias), `{git: {url, ref, subdir, auth, submodules}}` (`ref` default `main`), or `{database: {engine, version, database, user}}` ([Databases](databases.md)). |
| `build` | Git sources only: `{builder: {type: railpack \| nixpacks \| dockerfile (path, target) \| buildpacks (builder)}, args: {K: V}, untrusted: true}`. `untrusted` (the default) builds in a VM. The `buildpacks` builder is refused at build time ([Builds](builds.md#building)). |
| `env` | `.env` text, or a map `{KEY: value \| {secret: NAME}}` ([below](#the-environment-editor)). |
| `domains` | `[{host, path?, port?, https?, redirect?}]` for the ingress ([Domains and ingress](domains.md)); `port` defaults to the app's `port`. |
| `volumes` | Named volumes, `NAME:/path[:ro]` (`NAME` is `[a-z0-9-]`), shared by the app's replicas. Each is the app's own: the stack volume `<app>_<NAME>`, which is the incus volume `<project>-<env>_<app>_<NAME>`. Host paths are not allowed. Snapshots, backups and staged restores: [Volumes](volumes.md). |
| `ports` | Published host ports in compose syntax (`127.0.0.1:8080:80`), load-balanced over healthy replicas. |
| `replicas` | Default 1 (0 to 100). |
| `port` | The port the app listens on. |
| `healthcheck` | A compose `healthcheck` (`test`, `interval`, `timeout`, `retries`, `start_period`). Without one, a running replica is in rotation. |
| `resources` | `{cpus, memory}` per replica. |
| `command` | argv, or a line split like a shell would. |
| `previews` | Preview deployments per pull request: see [Preview deployments](previews.md). |
| `files` | `[{path, secret, mode?}]`: the org secret `secret`'s value as a file at the absolute `path` (config files, certificates), delivered like a stack's file secrets ([Secrets](secrets.md#stacks)). Mode default `0400`, owned by the app's numeric user or root. |
| `user` | The user the app runs as; numeric (`uid[:gid]`) on an OCI image. |
| `working_dir` | The working directory. |

Changing a setting (`app_update`, a JSON merge patch where `null` clears a
field; `isb app update NAME -f patch.yaml`, JSON or YAML) takes effect at the
next deploy. An app rolls out `start-first` (no gap), or `stop-first` when it
has volumes, since two live copies of a database on one volume do not mix.

The rendered service is labelled `isb.app=<name>`. An env secret `NAME`
becomes the stack secret `<app>.NAME` (`external`, store name `NAME`), so
`isb secret set NAME` rolls the app like any stack using it
([Secrets](secrets.md#stacks)).

**Domains** are stored with the app and passed to its service as the
stack's `domains:` list, so the ingress routes them at the next deploy;
`app_get` reports `domains_served: true`. A domain from `--domain HOST[/PATH]`
is served on `--port`.

## An app as YAML

Every setting above is one document: `app_export` (and the app page's **YAML**
tab) shows it, and `app_apply` takes it back, the way `kubectl get -o yaml`
and `kubectl apply -f` do. It holds the fields `app_create` takes, with secrets
by name only (`${{secret.NAME}}` in `env`, never a value):

```yaml
name: web
project: shop
environment: production
source:
  image: docker:traefik/whoami:latest
env: |
  GREETING=hi
  TOKEN=${{secret.api-token}}
replicas: 3
port: 80
```

Applying is declarative: the document is the app's whole desired settings, so
a field you remove goes back to its default (unlike `app_update`, which merges
a patch). A name that is new creates the app, in an existing project and
environment; a name that exists updates it. An app's name, project and
environment, and a database's engine, database and user, cannot change: that
is a different app. A document pasted from `app_get` is accepted (its
read-only fields, such as `stack` and `env_vars`, are ignored), and JSON works
as well as YAML.

```text
app_apply {definition: "<yaml>", dry_run: true}
  -> {valid, errors: [{line, column, message}], action: created|updated|unchanged,
      changes: ["env", "replicas"], diff: "--- current\n+++ proposed\n..."}
app_apply {definition: "<yaml>", deploy: true}     # apply, then deploy
```

`dry_run` checks everything a real apply checks (the fields, the project and
environment, the secrets it names) and writes nothing; a bad document is the
answer there, with the line it is on where it can be placed, and an error
otherwise. `deploy: true` queues a deployment after the apply, also when
nothing changed. A created app's answer carries its webhook secret. Applying
takes effect at the next deploy, like `app_update`. Members and up can apply;
viewers can export. Calls are audited under the app's name.

### The YAML tab

The **YAML** tab edits that document in a code editor (CodeMirror, loaded
when the tab opens): line numbers, folding, highlighting, Ctrl/Cmd-S. As you
type, the daemon checks the text (`app_apply` with `dry_run`) and marks the
problems on their lines; the **Changes** view is a line diff against the
saved definition. **Save** and **Save and deploy** first show the diff for
review. Save and deploy opens the deployment's live page, like Deploy does.
Viewers read the document and cannot edit it. A document that names another
app would create it, so the tab refuses it.

## The environment editor

`isb app env NAME` (tool `app_env_get`) prints the app's environment as
`.env` text, and `isb app env-set NAME [FILE]` (`app_env_set`, stdin without
a file) replaces it, like Dokploy's environment tab:

```sh
# database
DATABASE_HOST=db.shop-production
export PORT=8080            # `export` is accepted and dropped
GREETING="hello world"      # double quotes: \n \t \" \\ escapes, may span lines
RAW='kept # as is'          # single quotes: literal
API_TOKEN=${{secret.api_token}}
```

Comments and blank lines are kept where they are; a value is quoted on output
when it needs it. `KEY=${{secret.NAME}}` (unquoted) refers to the org secret
`NAME`, which must exist; the editor only ever shows the reference, never the
value. A quoted `${{secret...}}` is literal text. Plain values are instance
config (readable by whoever can read the instance, as in any compose file):
put anything sensitive in a secret.

`isb app create -e KEY=VALUE` (repeatable) and `--env-from FILE` set the
environment at creation.

## Deployments

```text
isb app deploy NAME [-d]                   follows the deployment's log; exit 0 when done
isb app rollback NAME [ID] [-d]            a previous deployment's image and settings, no build
isb app deployments NAME [--json] | logs NAME [ID] [-f]
```

Every deploy is a record: an id (1, 2, ... per app), what triggered it
(`manual` from the local CLI, `api` from a tool call, `webhook`), who, the
commit SHA and subject for a git source, the image and its digest, the status,
timestamps, and a log.

```text
queued → building → deploying → done
   │         └──────────┴──────→ failed
   └→ superseded
```

- **building** resolves the image: an image source's digest is looked up
  (`skopeo inspect`, when installed) and the app runs pinned to it
  (`docker:traefik/whoami@sha256:...`), so a moved tag never changes a
  running app behind its back and a rollback gets exactly the old image. A git
  source is fetched and built ([below](#git-sources)).
- **deploying** renders the app into its stack in place of its old service,
  hands the stack to the controller, and waits (up to 15 minutes) for that
  service to converge. A paused or failing rollout fails the deployment, with
  the controller's reason. A deploy whose settings did not change replaces the
  instances anyway (a restart), as Dokploy's deploy button does.
- **One deploy runs at a time per app.** A deploy asked for while one runs
  waits behind it; a newer request replaces a waiting one that has not
  started (it becomes `superseded`), so a burst of pushes builds the latest
  commit once.
- The last 30 records per app are kept, with their logs. A deployment the
  daemon was running when it stopped is marked failed when it starts again.

A deployment's log holds its git, build and image lines and, while it rolls
out, the controller's events about the service (slots created, probed,
serving, old ones drained), so it tells the whole deploy in one place.

The log is streamed two ways: `app_deployment_log` returns it from a byte
offset (poll with the returned `offset` until `finished`; `isb app logs NAME
[ID] -f` does) together with the deployment's record (`deployment`: status,
image, commit, timings, read before the text, so a finished record means the
text is complete), and each line is an event on the daemon's feed (`events`,
`GET /api/v1/events`) at level `log`, under the app's stack and service, next
to the controller's own rollout events.

**Rollback** (`isb app rollback NAME [ID]`, `app_rollback`) queues a
deployment that puts back what deployment ID ran (default: the last `done`
one before the current): its image, by digest, and the settings it was
deployed with, without building. The app's saved settings are not changed,
so the next deploy applies them again.

## Git sources

```console
$ isb app create api --project shop --git git@github.com:acme/api.git --ref main \
    --builder dockerfile
$ isb app deploy-key api            # prints the public key: add it to the repo's deploy keys
$ isb app deploy api
```

The daemon fetches the repository on the host into
`<state>/sources/<app>/` (`<state>/orgs/<org>/sources/<app>/` outside the
default org), checks out the exact commit the ref names, records its SHA and
subject, and calls the builder
([`isb::build`](https://github.com/execution-associates/isb/blob/main/crates/isb-apps/src/build/mod.rs)) with the checkout,
the `subdir`, the builder and its args, and the SHA as the tag. The build
runs in a fresh sandbox in the org, never on the host ([Builds](builds.md)).

The repository is untrusted, so git is held to a fetch and a checkout:

- URLs: `https://`, `http://` and `git://` (no credentials), `ssh://` and
  `git@host:owner/repo`. Local paths, `file://`, `ext::` and anything starting
  with `-` are refused, and git's own protocol allowlist is set to those four.
- No hooks (`core.hooksPath=/dev/null`), no fsmonitor, no system or global git
  config, no credential helpers, no prompts (`GIT_TERMINAL_PROMPT=0`), an
  environment cleared to what git and ssh need, and 10 minutes per git
  command. Submodules only with `submodules: true`.
- A `ref` is checked against git's ref rules; a pinned SHA must be what was
  fetched; a `subdir` must be a directory inside the checkout.

Credentials are org secrets, named in `source.git.auth`, and never in argv:

| `auth` | How it is used |
|---|---|
| `{token_secret: NAME, username?}` | HTTPS only. Sent as basic auth (`username` default `x-access-token`; GitHub, GitLab and Gitea tokens all work) in an `http.<origin>.extraHeader` set through `GIT_CONFIG_*` environment variables, so only the repository's origin ever receives it. |
| `{ssh_key_secret: NAME}` | SSH only. Written to a 0600 file in a 0700 temporary directory for the one git call, used through `GIT_SSH_COMMAND` with `-o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new` and a per-app `known_hosts`, so the first fetch pins the host key. |

`isb app deploy-key NAME` (`app_deploy_key`) generates an ed25519 key with
`ssh-keygen`, stores the private half as `app.<name>.deploy-key`, makes it the
app's credential and prints the public half.

## Webhooks

Every app has a webhook, served on the daemon's TCP listener without a
session:

```text
POST /api/v1/webhooks/<org>/<app>
```

Its credential is the app's webhook secret, generated when the app is created
and stored as the org secret `app.<app>.webhook`. `isb app webhook NAME`
(`app_webhook`) shows it; `--rotate` (`rotate: true`) makes a new one.

| Sender | Configure | Checked |
|---|---|---|
| GitHub | content type `application/json`, secret = the webhook secret | `X-Hub-Signature-256`: HMAC-SHA256 of the body |
| Gitea, Forgejo | secret = the webhook secret | `X-Gitea-Signature` / `X-Forgejo-Signature`: HMAC-SHA256 of the body |
| GitLab | secret token = the webhook secret | `X-Gitlab-Token`, compared in constant time |
| anything else | `?token=<secret>` | compared in constant time |

- A missing or wrong signature, an unknown org or an unknown app all answer
  401 and do nothing, so the endpoint reveals nothing about what exists.
- `ping` answers 200. Pull (merge) request events drive the app's
  [previews](previews.md) when it has them on. Other events (issues,
  comments) answer 200 and are ignored. A delivery id seen before
  (`X-GitHub-Delivery`, ...) is ignored.
- A push deploys a git app only when its ref matches the app's `ref`: branch
  `main` matches `refs/heads/main` (or a tag `main`), a full `refs/...` ref
  matches only itself, and a pinned SHA never deploys from a push. A branch
  deletion is ignored. The push's ref, SHA and commit subject are recorded on
  the deployment as `requested`.
- For an image app, any authenticated call (a registry's webhook, a CI job
  with `?token=`) deploys, pulling the tag's current digest.
- A deploy answers 202 with the deployment id.
- Every delivery is in the [audit log](../operations/audit.md) as
  `webhook.deploy`.

With Cloudflare Access in front of the daemon, the webhook path still needs to
reach it: add an Access bypass policy for `/api/v1/webhooks/*` (the daemon
serves that path ahead of its Access check, since each request is signed).
See [Reach isb serve remotely](remote-access.md).

## Tools

| Tool | Does |
|---|---|
| `project_create`, `project_list`, `project_delete` | Projects; `project_list` shows each environment's stack and apps. A project with apps cannot be deleted. |
| `environment_create`, `environment_list`, `environment_delete` | A project's environments (stored with the project). One with apps cannot be deleted. |
| `app_create` | Create an app (`deploy: true` deploys it too). Returns the app and its webhook secret. |
| `app_get`, `app_list` | Settings, stack, service name, current deployment, webhook path, `domains_served`; env as a map with `{secret: NAME}` references. |
| `app_update` | A merge patch of settings (`deploy: true` deploys after). |
| `app_export`, `app_apply` | The app as a YAML document, and declarative create-or-update from one (`dry_run`, `deploy`); see [An app as YAML](#an-app-as-yaml). |
| `app_delete` | Its service leaves the stack (the stack goes with its last app); its records, checkout, webhook secret and deploy key go. Named volumes are kept. |
| `app_deploy`, `app_rollback` | Queue a deployment; `wait: true` returns when it finishes. |
| `app_deployments`, `app_deployment_log` | History, and one deployment's log from an offset. |
| `app_env_get`, `app_env_set` | The environment as `.env` text. |
| `app_webhook`, `app_deploy_key` | The webhook path and secret (rotate); a new SSH deploy key. |

The full tool catalog is in [MCP tools](../reference/mcp-tools.md).

## On disk

```text
<state>/apps/projects/<project>.json          (<state>/orgs/<org>/apps/... in other orgs)
<state>/apps/<app>/app.json
<state>/apps/<app>/deployments/<id>.json, <id>.log
<state>/apps/<app>/previews/<n>/preview.json, deployments/<id>.json, <id>.log
<state>/sources/<app>/repo, known_hosts
```

Files are 0600 and written atomically (temporary file, fsync, rename).
