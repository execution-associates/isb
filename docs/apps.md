# Apps: projects, environments and deployments

Stacks ([stacks.md](stacks.md)) take a compose file. Apps are the layer above,
the way Dokploy presents it: you say what to run (an image, or a git
repository and how to build it) and its settings, and isb keeps the deploy
history, the logs and the webhooks.

```text
org → project → environment → app
```

- A **project** groups environments. It starts with one, `production`; add
  `staging`, `preview` and so on.
- An **environment** of a project runs as **one ordinary stack** named
  `<project>-<env>` (at most 30 characters), and each of its apps is **one
  service** in that stack. Apps reach each other by service name,
  `<app>.<project>-<env>` (or `<app>.<project>-<env>.<org>.isb`), as any
  stack's services do ([stacks.md](stacks.md#service-discovery)). `isb stack
  ps shop-production` shows the apps of project `shop`, environment
  `production`.
- An **app** is a source plus settings. Deploying it replaces its own service
  in the stack and nothing else: revisions are per service, so only that app
  rolls.

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
tools (below), so `POST /orgs/<org>/api/v1/tools/app_deploy` with an API
token deploys an app.

## An app's settings

| Field | What |
|---|---|
| `name` | `[a-z0-9-]`, unique in the org; the service name in its stack. Fixed. |
| `project`, `environment` | Where it runs (environment default `production`). Fixed. |
| `source` | `{image: REF}` (`docker:nginx:1.27`, `ghcr:org/app:tag`, a local alias), or `{git: {url, ref, subdir, auth, submodules}}`. |
| `build` | Git sources only: `{builder: {type: railpack \| nixpacks \| dockerfile (path, target) \| buildpacks (builder)}, args: {K: V}, untrusted: true}`. `untrusted` (the default) builds in a VM. |
| `env` | `.env` text, or a map `{KEY: value \| {secret: NAME}}`. |
| `domains` | `[{host, path?, port?, https?, redirect?}]` for the ingress; `port` defaults to the app's `port`. |
| `volumes` | Named volumes, `NAME:/path[:ro]`: the incus volume `<stack>_<app>_<NAME>`, shared by the app's replicas. Host paths are not allowed. |
| `ports` | Published host ports in compose syntax (`127.0.0.1:8080:80`), load-balanced over healthy replicas. |
| `replicas` | Default 1. |
| `port` | The port the app listens on. |
| `healthcheck` | A compose `healthcheck`. Without one, a running replica is in rotation. |
| `resources` | `{cpus, memory}` per replica. |
| `command` | argv, or a line split like a shell would. |
| `files` | `[{path, secret, mode?}]`: the org secret `secret`'s value as a file at `path` (config files, certificates), delivered like a stack's file secrets ([secrets.md](secrets.md#stacks)). Mode default `0400`. |
| `user` | The user the app runs as; numeric (`uid[:gid]`) on an OCI image. |
| `working_dir` | The working directory. |

Changing a setting (`app_update`, a JSON merge patch where `null` clears a
field; `isb app update NAME -f patch.yaml`) takes effect at the next deploy.
An app rolls out `start-first` (no gap), or `stop-first` when it has volumes,
since two live copies of a database on one volume do not mix.

The rendered service is labelled `isb.app=<name>`. An env secret `NAME`
becomes the stack secret `<app>.NAME` (`external`, store name `NAME`), so
`isb secret set NAME` rolls the app like any stack using it
([secrets.md](secrets.md#stacks)).

**Domains** are stored with the app and passed to the service as the
contract's `domains:` list once the compose parser takes that key (the
ingress adds it). Until then the deploy log notes that they are kept but not
served, and `app_get` reports `domains_served: false`.

## The environment editor

`isb app env NAME` (tool `app_env_get`) prints the app's environment as
`.env` text, and `isb app env-set NAME [FILE]` (`app_env_set`) replaces it,
like Dokploy's environment tab:

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
value. Plain values are instance config (readable by whoever can read the
instance, as in any compose file): put anything sensitive in a secret.

## Deployments

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
  source is fetched and built (below).
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

The log is streamed two ways: `app_deployment_log` returns it from a byte
offset (poll with the returned `offset` until `finished`; `isb app logs NAME
[ID] -f` does), and each line is an event on the daemon's feed (`events`,
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
subject, and calls the builder ([`isb::build`](../src/build/mod.rs)) with the
checkout, the `subdir`, the builder and its args, and the SHA as the tag. The
build runs in a fresh sandbox in the org, never on the host.

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

Every app has a webhook, served on the TCP listener without a session:

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
- `ping` answers 200. Other events (issues, pull requests) answer 200 and are
  ignored. A delivery id seen before (`X-GitHub-Delivery`, ...) is ignored.
- A push deploys a git app only when its ref matches the app's `ref`: branch
  `main` matches `refs/heads/main` (or a tag `main`), a full `refs/...` ref
  matches only itself, and a pinned SHA never deploys from a push. A branch
  deletion is ignored. The push's ref, SHA and commit subject are recorded on
  the deployment as `requested`.
- For an image app, any authenticated call (a registry's webhook, a CI job
  with `?token=`) deploys, pulling the tag's current digest.
- A deploy answers 202 with the deployment id.

With Cloudflare Access in front of the daemon, the webhook path still needs to
reach it: add an Access bypass policy for `/api/v1/webhooks/*` (the daemon
serves that path ahead of its Access check, since each request is signed).

## Tools

| Tool | Does |
|---|---|
| `project_create`, `project_list`, `project_delete` | Projects; `project_list` shows each environment's stack and apps. A project with apps cannot be deleted. |
| `environment_create`, `environment_list`, `environment_delete` | A project's environments (stored with the project). One with apps cannot be deleted. |
| `app_create` | Create an app (`deploy: true` deploys it too). Returns the app and its webhook secret. |
| `app_get`, `app_list` | Settings, stack, service name, current deployment, webhook path; env as a map with `{secret: NAME}` references. |
| `app_update` | A merge patch of settings (`deploy: true` deploys after). |
| `app_delete` | Its service leaves the stack (the stack goes with its last app); its records, checkout, webhook secret and deploy key go. Named volumes are kept. |
| `app_deploy`, `app_rollback` | Queue a deployment; `wait: true` returns when it finishes. |
| `app_deployments`, `app_deployment_log` | History, and one deployment's log from an offset. |
| `app_env_get`, `app_env_set` | The environment as `.env` text. |
| `app_webhook`, `app_deploy_key` | The webhook path and secret (rotate); a new SSH deploy key. |

## On disk

```text
<state>/apps/projects/<project>.json          (<state>/orgs/<org>/apps/... in other orgs)
<state>/apps/<app>/app.json
<state>/apps/<app>/deployments/<id>.json, <id>.log
<state>/sources/<app>/repo, known_hosts
```

Files are 0600 and written atomically (temporary file, fsync, rename).
