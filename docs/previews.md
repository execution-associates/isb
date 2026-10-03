# Preview deployments

An app with a git source can get a **preview per pull request**: the request's
head built and served on its own URL while the request is open, and removed
when it closes or merges, as Dokploy's preview deployments do.

```yaml
# isb app update web -f previews.yaml
previews:
  enabled: true
  branches: [main]          # base branches watched (default: the app's ref)
  max: 3                    # previews at once
  env: |
    MODE=preview
    DATABASE_URL=postgres://db.shop-preview/app
    API_KEY=${{secret.preview_api_key}}
  domain: auto              # or *.preview.example.com
  status:                   # optional: a commit status with the URL
    token_secret: forge-token
```

```console
$ isb app previews ls web
APP  PR  STATUS  HEAD     COMMIT    URL
web  #1  done    feature  0ba71049  https://web-shop-production-pr-1-acme.203-0-113-10.sslip.io/
$ isb app previews show web 1
$ isb app previews logs web 1 -f
$ isb app previews redeploy web 1
$ isb app previews rm web 1
```

## What triggers what

Previews ride on the app's webhook (`POST /api/v1/webhooks/<org>/<app>`,
[apps.md](apps.md#webhooks)) with its signature checks. Subscribe the webhook
to pull request events too: GitHub `pull_request`, Gitea/Forgejo "Pull
Request" events, GitLab "Merge request events". A `?token=` call carries no
pull requests.

| Event | GitHub | Gitea, Forgejo | GitLab | Does |
|---|---|---|---|---|
| opened, reopened | `opened`, `reopened` | `opened`, `reopened` | `open`, `reopen` | create the preview, build, deploy |
| new commits | `synchronize` | `synchronized` | `update` with `oldrev` | build and roll the preview |
| closed | `closed` | `closed` | `close`, `merge` | remove the preview |
| anything else | | | | answered 200, ignored |

- Only requests whose base branch is in `branches` (default: the app's git
  ref) get a preview; closing removes one whatever its base.
- A new pull request over `max` gets none (`ignored`, and an `error` commit
  status when `status` is set) until another preview goes.
- A synchronize naming the commit the preview already has does nothing
  (Gitea sends one right after opening).
- Deploys queue per preview like an app's: one at a time, a newer one
  replacing a waiting one.

The head is fetched with the app's git source, URL and credentials, and the
same hardened git ([apps.md](apps.md#git-sources)), from the ref the forge
keeps for the request in the base repository: `refs/pull/<n>/head` (GitHub,
Gitea, Forgejo) or `refs/merge-requests/<n>/head` (GitLab). A fork's commits
are fetched from the base repository, never from the fork.

## Isolated from production

| | Production | Preview of pull request `n` |
|---|---|---|
| stack | `<project>-<env>` | `<project>-<env>-pr-<n>` (`<project>-pr-<n>` when that is too long), same org |
| service | `<app>` | `<app>`, labelled `isb.preview=<n>`; apps of one environment previewing the same number share the stack and reach each other as `<app>.<project>-<env>-pr-<n>` |
| image | `registry:<app>:<sha>` | `registry:<app>:pr-<n>-<sha>` |
| volumes | `<app>_<name>` | `<stack>_<app>_<name>`: fresh, its own |
| environment | the app's `env` | `previews.env` only; the app's too (under it) with `inherit_env: true` |
| published host ports | the app's `ports` | none |
| replicas, resources | the app's | `previews.replicas` (default 1), `previews.resources` (default the app's) |
| build cache | `build-cache-<app>` | `build-cache-<app>-preview`; a fork's `build-cache-<app>-pr-<n>` |
| domain | the app's `domains` | `previews.domain` on `previews.port` (default the app's port) |

So a preview starts with an empty environment: it never points at
production's database or receives production's secrets unless `env` (or
`inherit_env`) says so. Environment names ending in `pr-<number>` are refused,
so no environment's stack can be mistaken for a preview's.

### Domains

- `auto` (default): the ingress' generated name
  ([ingress.md](ingress.md#generated-names)) for the preview's stack,
  `<app>-<project>-<env>-pr-<n>-<org>.<a-b-c-d>.sslip.io`.
- `*.<suffix>`: `<app>-pr-<n>.<suffix>`, a concrete name that must be within
  the org's allowed domains (`<suffix>` or a parent of it). Point a wildcard
  DNS record at the server (or the org's tunnel).

The preview's URL (from the ingress, once it serves it) is on the preview
record (`url`), in the `deploy.succeeded` event and in the commit status.

## Pull requests from forks

A fork's pull request runs code nobody with push access wrote, so by default
it gets **no preview** (`ignored ... previews for forks are off`). With
`forks: true` it does, and:

- it builds in a **VM**, whatever the app's `build.untrusted`, with a build
  cache of its own that goes with the preview;
- it receives **none of the app's secrets**: with `inherit_env` the app's
  plain values come along, its secret references do not;
- of the secrets in `previews.env`, only those named in `fork_secrets` reach
  it; the others are left out (the deployment log says which).

A request counts as from a fork when its head repository is not the base
repository (GitHub, Gitea: the repositories' full names; GitLab: the project
ids), or is unknown (a deleted fork). Once a fork, a preview stays one.

## Commit statuses

With `status: {token_secret: NAME}` (an org secret holding an API token that
may write commit statuses), each preview deploy posts the commit status
`isb/preview` on the head commit: `pending` while it builds, then `success`
with the preview's URL as the link, or `failure` with the error. A status with
the same context replaces the last one.

| `kind` | API | Token header |
|---|---|---|
| `github` | `POST /repos/{owner}/{repo}/statuses/{sha}` on `https://api.github.com` (`https://<host>/api/v3` for GitHub Enterprise) | `Authorization: Bearer` |
| `gitea` (also Forgejo) | the same path on `https://<host>/api/v1` | `Authorization: token` |

`kind` defaults to the webhook's sender. The API base comes from the app's
git URL (owner and repository from its path); set `api_url` for anything
else. The token is only sent over HTTPS unless `api_url` is set to an
`http://` URL explicitly. GitLab merge requests get no status. A status that
cannot be posted is a note in the deployment log, never a failed deploy.

## Removal and cleanup

Closing or merging the pull request, `preview_delete` (`isb app previews
rm`), a `ttl`, or deleting the app removes a preview:

1. a deploy waiting to start is superseded; one running finishes first;
2. its service leaves the preview stack (the stack goes with its last
   service, `isb stack rm --volumes` style);
3. its volumes are deleted (retried while instances wind down), and a fork's
   build cache;
4. its images: every `pr-<n>-*` manifest in `<org>/<app>` that no deployed
   service runs (the registry deletes manifests, not tags, so a digest a
   production tag shares stays); the blobs go at the next `isb registry gc`;
5. its records and checkout.

A removal that fails is retried every five minutes by the daemon, which also
removes previews not updated within `ttl` (`7d`, `36h`; at least `10m`;
default none: kept until the pull request closes). Each preview deploy also
deletes that preview's older images. Registry retention
([builds.md](builds.md#retention)) never counts `pr-*` tags among an app's
newest: a preview's image is kept while it is deployed and no longer.

Turning `enabled` off stops new previews and redeploys; existing ones stay
until their pull requests close or they are deleted.

## Events

On the events feed, under the preview's stack (`<org>/<project>-<env>-pr-<n>`)
and the app's service: `preview.created`, `deploy.succeeded` / `deploy.failed`
(`app web preview #3: deployment 2 done: https://...`), `preview.removed`,
and each deployment log line at level `log`.

## Settings

| Field | Default | |
|---|---|---|
| `enabled` | `false` | |
| `branches` | the app's ref | base branches watched |
| `max` | 3 | previews at once (1 to 50) |
| `env` | empty | `.env` text or `{KEY: value \| {secret: NAME}}` |
| `inherit_env` | `false` | start from the app's environment |
| `domain` | `auto` | `auto` or `*.<suffix>` |
| `port` | the app's | |
| `replicas` | 1 | 1 to 10 |
| `resources` | the app's | `{cpus, memory}` |
| `ttl` | none | remove when not updated for this long |
| `forks` | `false` | previews for forks' pull requests |
| `fork_secrets` | none | secrets in `env` a fork's preview may get |
| `status` | none | `{token_secret, kind?, api_url?}` |

## Tools

| Tool | Does |
|---|---|
| `preview_list` | Previews of an app (`name`) or of every app in the org, with status and URL. |
| `preview_get` | One (`name`, `number`), with its deployments. |
| `preview_log` | A preview deployment's log from `offset`. |
| `preview_redeploy` | Fetch the head again, build, roll (`wait`). |
| `preview_delete` | Remove one now (`wait`). |

Settings change with `app_update` (`{"name": "web", "previews": {...}}`, a
merge patch like every app setting).

## On disk

```text
<state>/apps/<app>/previews/<n>/preview.json
<state>/apps/<app>/previews/<n>/deployments/<id>.json, <id>.log   (last 10)
<state>/sources/<app>/previews/<n>/repo
```

## Reaching the webhook

The forge must reach the daemon's TCP listener, which binds loopback only:
in production that is a Cloudflare Tunnel (with an Access bypass for
`/api/v1/webhooks/*`, [apps.md](apps.md#webhooks)) or a reverse proxy. Its
verification on titan used a forge running in another org: a second Caddy
on that org's bridge address passed only `/api/v1/webhooks/*` to the
daemon's loopback listener, behind a one-port ufw rule on that bridge.
