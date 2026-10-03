---
title: "Templates: one-click apps"
description: Deploy ready-made sets of apps (Uptime Kuma, Gitea, Umami and more, or Dokploy's catalog) with settings filled in and passwords generated.
order: 4
nav_title: Templates
---

A template is a ready-made set of apps (Uptime Kuma, Gitea with its
Postgres, ...) with the settings filled in and the passwords generated, so a
working service is one command away. Deploying one into a project
environment creates ordinary [apps](deploy-apps.md), so everything an app has
(deployments, rollback, the environment editor, webhooks, the web UI's app
pages) works for them. isb ships its own catalog and can read Dokploy's.

```console
$ isb template ls analytics
REF               NAME                 TAGS       DESCRIPTION
builtin/plausible Plausible Analytics  analytics  Privacy-friendly web analytics ...
builtin/umami     Umami                analytics  Simple, privacy-focused web analytics, ...
$ isb template show umami                      # variables, apps, notes
$ isb --org acme template deploy umami --project web --dry-run
would deploy builtin/umami as umami in web/production (stack web-production)
  app umami-db: docker:postgres:16-alpine
  app umami: ghcr:umami-software/umami:postgresql-latest
  secret tpl.umami.db_password (variable db_password)
  secret tpl.umami.app_secret (variable app_secret)
  secret tpl.umami.umami.env.DATABASE_URL (app umami env DATABASE_URL)
  domain = umami-web-production-acme.203-0-113-7.sslip.io (generated)
  url https://umami-web-production-acme.203-0-113-7.sslip.io/
$ isb --org acme template deploy umami --project web -s domain=stats.example.com
...
umami-db: done
umami: done
```

```text
isb template ls [WORDS...] [--tag T] [--catalog C] [--json]
isb template show REF [--json]             variables, apps, notes; a Dokploy template's translation report
isb template deploy REF --project P [--env E] [--name N] [-s K=V]... [--dry-run] [-d] [--json]
isb template instances [--json] | rm NAME  deployed templates; rm deletes their apps (volumes kept) and secrets
isb template catalog ls | add NAME --format native|dokploy LOCATION | rm NAME   (add, rm: platform admins)
```

All take `--org ORG`. The web UI's **Templates** section does the same, with a
form generated from each template's variables
([The web UI](../getting-started/web-ui.md)).

## Deploying

`template_deploy` (`isb template deploy REF --project P`) takes:

| Argument | |
|---|---|
| `template` | `catalog/id`, or a bare id (the built-in catalog first, then the added ones). |
| `project`, `environment` | Where the apps go (environment default `production`). A missing project or environment is created. |
| `name` | The instance name (default: the template id). It names the apps and the secrets, so a template can be deployed twice in an org under two names. |
| `values` | Variable values (`-s KEY=VALUE`). Generated variables may be left out; a text variable without a default is required. |
| `dry_run` | Return the plan and change nothing. |
| `wait` | Return once every app has deployed (the CLI waits unless `-d`). |
| `timeout` | How long `wait` waits. |

What happens:

1. **Variables** get their values: the deployer's, else a generator's
   (passwords, keys, ids), else the default. A `domain` left empty is a
   generated name (`host: auto`, `<app>-<stack>-<org>.<ip>.sslip.io`; it
   needs the server's public address, `isb serve --ingress-public-ip` or
   detected).
2. **Apps** are named `<name>-<key>` for each app of the template; the main
   app is just `<name>`. Apps reach each other by service name,
   `<app>.<project>-<env>` ([service discovery](../concepts/stacks.md#service-discovery));
   since a host's legacy `default` org has no service names
   ([Orgs](../concepts/orgs.md#what-an-org-is-in-incus)), a template with
   more than one app needs a real org there.
3. **Secrets** go into the org's store, labelled `isb.template` and
   `isb.template.instance`:
   - each secret variable as `tpl.<name>.<var>`;
   - a value built from one (`postgres://u:${password}@...`) as
     `tpl.<name>.<app>.env.<KEY>`;
   - each file as `tpl.<name>.<app>.file<N>`.

   App definitions hold only `{secret: NAME}` references. A secret in a
   command line or health check reaches the process as a variable
   (`ISB_TPL_<VAR>`) through `/bin/sh -c`, never in the stored command.
4. **Deploys** run one app after another in dependency order, each waiting
   for the one before to converge (a database is healthy before its app
   starts). A failure stops the rest; `isb app deployments NAME` has the log.
   Without `wait`, the first app's deployment is queued before
   `template_deploy` answers, and its id comes back as `first_deployment`
   (`{app, id}`), so a client can follow its log from the first line.

Nothing is created when anything is in the way (an app or secret of the same
name, a variable that does not validate). A failure while creating removes
what was created.

`template_instance_list` (`isb template instances`) lists what was deployed:
template, project, environment, apps, secret names, non-secret values and
URLs. `template_instance_delete` (`isb template rm NAME`) deletes the apps
(their named volumes are kept, as `app_delete` keeps them), the
`tpl.<name>.*` secrets and the record. The instance record lives in
`<state>/templates/instances/<name>.json`
(`<state>/orgs/<org>/templates/instances/` outside the default org).

Values derived from a secret are copies: after `isb secret set
tpl.umami.db_password`, also update `tpl.umami.umami.env.DATABASE_URL` (and
the database itself).

## The format

A template is a YAML (or JSON) document:

```yaml
id: umami                     # [a-z0-9-]
name: Umami
description: Simple, privacy-focused web analytics, with a Postgres database.
version: "2"                  # the template's own version
logo: https://...             # optional: an https image URL (see Logos)
tags: [analytics]
links: {website: https://umami.is, source: https://github.com/umami-software/umami}
variables:
  - name: domain
    type: domain
    description: The public hostname; empty for a generated one.
  - name: db_password
    type: password
  - name: app_secret
    type: hex
    bytes: 32
apps:
  - name: db
    image: docker:postgres:16-alpine
    volumes: ["data:/var/lib/postgresql/data"]
    env:
      POSTGRES_USER: umami
      POSTGRES_PASSWORD: "${db_password}"
    healthcheck: {test: [CMD-SHELL, "pg_isready -U umami"], interval: 5s}
  - name: umami
    image: ghcr:umami-software/umami:postgresql-latest
    port: 3000
    depends_on: [db]
    env:
      DATABASE_URL: "postgresql://umami:${db_password}@${host:db}:5432/umami"
      APP_SECRET: "${app_secret}"
    domains:
      - {host: "${domain}"}
main: umami                   # named after the instance (default: the only app)
notes:
  - Sign in as admin / umami and change the password at once.
```

### Strings

Every string may use:

| | |
|---|---|
| `${var}` | A variable's value. |
| `${host:KEY}` | App `KEY`'s service name, `<app>.<project>-<env>`. |
| `$$` | A literal `$`. |

Any other `$` is literal (`$HOME` in a shell line stays `$HOME`); any other
`${...}` is an error. A secret variable may appear in `env`, `files`,
`command`/`args` and `healthcheck`, not in an image, a volume, a port or a
domain.

### Variables

| Field | |
|---|---|
| `name` | `[a-z][a-z0-9_]*`. |
| `type` | Below; default `string`. |
| `label`, `description` | For forms. |
| `default` | Used when the deployer gives nothing; may use `${...}`. |
| `value` | A computed value from other variables; not an input. |
| `required` | Default: a text variable (`string`, `email`, `url`, `int`) without a default. |
| `choices`, `min_length`, `max_length`, `min`, `max` | Validation of what the deployer gives. |
| `secret` | Store it as an org secret. Default: `password`, `base64`, `hex`, `jwt`; a computed value that uses a secret is one too. |

| `type` | Value |
|---|---|
| `string`, `email`, `url`, `int` | Given (validated by type), or the default. |
| `domain` | A hostname; empty for a generated one. Used as a domain's `host` by one app, it stays `host: auto`; shared by several apps, the generated name is written out (an org with a domain allowlist then needs it listed). |
| `password` | Generated: `length` (32) letters and digits. |
| `base64` | Generated: `bytes` (32) random bytes, base64. |
| `hex` | Generated: `bytes` (32) random bytes, hex. |
| `uuid` | Generated: a random UUID. |
| `username` | Generated: `length` (8) lowercase letters. |
| `port` | Generated: a free TCP port on the host. |
| `timestamp` | Generated: now (or `at: 2030-01-01T00:00:00Z`), in seconds (`unit: ms` for milliseconds). |
| `jwt` | Generated: an HS256 JWT signed with the variable `jwt.secret`, claims `jwt.payload` (a JSON object expression; `iat` and `exp`, ten years on, are added when missing). |

Any variable may be given by the deployer; a generated one then takes that
value.

### Apps

Each app is an [app](deploy-apps.md#an-apps-settings) with an image source:
`name` (the key), `image` (an isb image reference: `docker:`, `ghcr:`,
`quay:`, `oci:`), `env` (a map), `port`, `domains`, `volumes` (named,
`NAME:/path[:ro]`), `ports`, `replicas`, `healthcheck`, `resources`, `user`,
`working_dir`, plus:

| Field | |
|---|---|
| `command` | The whole command line, as isb's `command` (it replaces the image's entrypoint too). |
| `args` | Arguments after the image's own entrypoint (docker's `command`): the entrypoint is read from the image (`skopeo inspect --config`) at deploy. |
| `files` | `[{path, content, mode?}]`: files in the app's instances. The content is rendered and stored as a secret; mode default `0444`. |
| `depends_on` | Apps deployed (and converged) first. |

A template cannot ask for anything an app cannot have: no host paths,
privileged mode, devices or extra capabilities. The org's own limits apply
on top.

## Catalogs

**Built in:** Uptime Kuma, Plausible Analytics (Postgres, ClickHouse),
Gitea (Postgres), n8n, Ghost (MySQL), Umami (Postgres), Vaultwarden, MinIO,
Postgres with Adminer, and whoami (for checking routing). They are written
for isb from each project's own documented images and settings, and compiled
into the binary
([`crates/isb-apps/src/template/builtin/`](https://github.com/execution-associates/isb/tree/main/crates/isb-apps/src/template/builtin)).

**Added catalogs** are a platform setting (`template_catalog_add`,
`isb template catalog add`; platform admins), kept in
`<state>/templates/catalogs.json`. `template_catalog_list` lists them for
every caller.

| `format` | `location` |
|---|---|
| `native` | A directory of `*.yaml` (or `<id>/template.yaml`), or an `https://` URL of one YAML/JSON document `{templates: [...]}`. |
| `dokploy` | A checkout of [Dokploy/templates](https://github.com/Dokploy/templates) (or its `blueprints/`), or `https://templates.dokploy.com`. |

```sh
isb template catalog add dokploy --format dokploy https://templates.dokploy.com
isb template show dokploy/ntfy
```

Listings are cached for ten minutes. URLs are fetched over https only, at
most 4 MiB a file. What a catalog holds is third-party data: it is parsed
and translated, never run on the host. A native template in a directory that
does not parse is skipped, and the daemon logs why.

## Dokploy templates

A Dokploy template (`docker-compose.yml` plus `template.toml`) is translated
into this format when it is fetched, strictly: anything that would change
what the app is, or weaken isolation, is either mapped faithfully or the
template is refused with the reason. `template_get` reports the outcome as
`compatibility`: `clean` (means the same here), `notes` (deployable; the
notes say what differs) or `refused`. Coming from Dokploy in general:
[Coming from Dokploy](from-dokploy.md).

**Mapped:**

- `[variables]` become variables: `${domain}` a `domain`, `${password:N}`
  a `password` (Dokploy's length, default 16), `${base64:N}` a `base64`,
  `${hash:N}` and `${jwt:N}` a `hex`, `${uuid}`, `${randomPort}`,
  `${username}`, `${email}` (a generated `user@example.com`, overridable),
  `${timestamp}`/`${timestampms}`/`${timestamps[:date]}`, and
  `${jwt:secret[:payload]}` a signed JWT. Literal values become defaults;
  helpers inside a value become variables of their own.
- `[config.env]` is Dokploy's `.env`: it feeds the compose file's `${VAR}`
  interpolation (`:-`, `-`, `:+`, `+`, `:?`, `?`) and `env_file: .env`.
- `[[config.domains]]` and Traefik router labels (`Host(...)`, optionally
  `&& PathPrefix(...)`, the service's `loadbalancer.server.port`, the
  `web`/`websecure` entrypoints, a `stripprefix` middleware) become
  `domains`.
- `[[config.mounts]]` mounted from `../files/...` become `files`; a
  `../files/` directory with no content, a path in the compose project
  (`./data`) and an anonymous volume become named volumes.
- `image` (Docker Hub, `ghcr.io`, `quay.io`, other registries as `oci:`),
  `environment`, named `volumes`, `command` (as `args`) and `entrypoint`,
  `healthcheck`, `depends_on`, numeric `user`, `working_dir`,
  `deploy.replicas` and `resources.limits`, `mem_limit`, `cpus` (rounded up
  to whole CPUs).
- Other services' names in env values, arguments, health checks and files
  are rewritten to their service names when they sit in a host position
  (`//db`, `@db`, `db:5432`, or a whole `*_HOST`/`*_URL`-style value).
  Names an image uses by default, unseen in the template, are not; that is
  the most likely reason a translated multi-service template fails to
  connect.

**Notes** (deployable, different): no restart policy or `on-failure`
(apps are always kept running); `ulimits`, `shm_size`, `tmpfs`,
`stop_signal`, `stop_grace_period`, `read_only`, `cap_drop`, `security_opt`
not applied (the app runs as an unprivileged incus container); a container
port without a host port is not published, and `HOST:CONTAINER` is published
on `127.0.0.1` only; the host's `/etc/localtime` and `/etc/timezone` are not
mounted; `hostname`, `container_name`, aliases and other labels are not set;
a template with several services needs a real org (they reach each other by
service name).

**Refused**, with the reason: `privileged`, `cap_add`, `devices`, host
`network_mode`/`pid`/`ipc`/`uts`/`userns_mode`, `sysctls`, `extra_hosts`,
custom DNS, another `runtime`, `volumes_from`, host paths (the docker socket
among them), volumes with driver options or other drivers, one named volume
shared by several services (an app's volumes are its own), `build`, compose
`secrets`/`configs`, one-shot jobs (`restart: "no"`,
`service_completed_successfully`), non-numeric `user`, published UDP ports
and ranges, Traefik TCP/UDP routing, rules beyond `Host`/`PathPrefix` and
other middlewares, and a `template.toml` that is not valid TOML.

## Logos

Template cards in the web UI show each template's logo. A browser never
loads a logo from its upstream URL: the page asks isb, and isb fetches,
checks and caches the image. So the UI's Content-Security-Policy keeps
`img-src` to isb's own origin, and a page view sends no request to anyone
else's server.

**Where a logo comes from:**

| Template | Logo URL |
|---|---|
| Built in | The template's `logo:`, an https link to an image in the project's own repository, pinned to a commit (whoami has none). |
| A native catalog | The template's `logo:` field, if any. Only an `https://` URL is ever fetched. |
| Dokploy, from a URL (`https://templates.dokploy.com`) | The file `meta.json` names, under the catalog: `<location>/blueprints/<id>/<logo file>`. |
| Dokploy, from a checkout | None: the cards show initials. |

**The endpoint:** `GET` (or `HEAD`) `/api/v1/templates/<catalog>/<id>/logo`.
Whoever may call `template_list` may read it (signed in as for any tool, with
the same authorizer). It answers the image, or `404 no logo` (with
`Cache-Control: no-store`) when the template has no logo or it could not be
fetched. The web UI asks for it only when the template lists a logo.

**Fetching** is third-party data, held to the same rules as a notification
webhook ([Notifications](notifications.md#private-destinations-ssrf)):
https only, every address checked against the SSRF policy (no loopback,
private, link-local or other non-public destination, whatever the
server-wide setting for notifications) and the connection made to the
address that was checked, at most three redirects and all on https,
15 seconds and 512 KiB at most, one fetch per URL at a time.

**What is kept:** only bytes that are a PNG, JPEG, GIF, WebP, ICO or SVG
image, decided from the bytes rather than the server's Content-Type. The
copy lives under `<state>/templates/logos/` (one file per URL, named by its
SHA-256) and is served for a week before it is fetched again.

**How it is served:** with the type isb decided,
`X-Content-Type-Options: nosniff`, `Cache-Control: private, max-age=86400`,
`Cross-Origin-Resource-Policy: same-origin`, and
`Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; sandbox`,
so an SVG opened directly runs no script.

**When it fails:** a logo that cannot be fetched is a 404 and is not tried
again for ten minutes (the daemon logs the host and the reason); while a
refresh fails, the stale copy is served. A template without a logo, or whose
logo does not load, shows its initials instead. A catalog added a moment ago
gets its logos within seconds (the template-to-logo index is rebuilt at most
once a minute, or after five seconds for a template it does not know).

## Tools

| Tool | Does |
|---|---|
| `template_list`, `template_get` | The built-in catalog and added ones (search by words, tag, catalog); a template's variables, apps, notes and, for a Dokploy template, its translation report. |
| `template_deploy` | Deploy a template into a project environment as apps (`template`, `project`, `environment`, `name`, `values`, `dry_run`, `wait`, `timeout`). `dry_run` returns the plan. |
| `template_instance_list`, `template_instance_delete` | Deployed templates; deleting one removes its apps (named volumes are kept) and the secrets it made. |
| `template_catalog_list`, `template_catalog_add`, `template_catalog_remove` | The catalogs added to the built-in one: a host directory or an https URL, isb's format or Dokploy's. Adding and removing: platform admins. |

## Licensing

isb's built-in templates are its own (MIT, like isb), written from each
project's public images and documentation. The applications they deploy are
under their own licenses.

The [Dokploy/templates](https://github.com/Dokploy/templates) repository is
MIT-licensed (Copyright (c) 2024 Dokploy and Carlos Ortiz); its templates
carry no licenses of their own. isb does not bundle it: a platform admin
adds it as a catalog, and isb fetches it at runtime from its public URL (or
reads a checkout). Its logos are the projects' trademarks, as are the
built-in templates' (each links to an image in the project's own repository
or site); isb links to them and does not copy them into its source.
