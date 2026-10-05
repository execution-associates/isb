---
title: Projects, environments and apps
description: The app layer over stacks, organized the way Dokploy organizes it.
order: 3
nav_title: Apps
---

[Stacks](stacks.md) take a compose file. Apps are the layer above, the way
Dokploy presents it: you say what to run (an image, a git repository and how
to build it, or a database engine) and its settings, and isb keeps the deploy
history, the logs and the webhooks. Most people who deploy services use apps;
stacks are there when you want to write the compose file yourself.

```text
org → project → environment → app
```

- A **project** groups environments. It starts with one, `production`; add
  `staging`, `preview` and so on.
- An **environment** of a project runs as **one ordinary stack** named
  `<project>-<env>` (at most 30 characters), and each of its apps is **one
  service** in that stack. Apps reach each other by service name,
  `<app>.<project>-<env>` (or `<app>.<project>-<env>.<org>.isb`), as any
  stack's services do ([Service discovery](stacks.md#service-discovery)).
  `isb stack ps shop-production` shows the apps of project `shop`,
  environment `production`.
- An **app** is a source plus settings. Deploying it replaces its own service
  in the stack and nothing else: revisions are per service, so only that app
  rolls.

## Sources

| Source | What isb does |
|---|---|
| An image (`docker:nginx:1.27`, `docker:traefik/whoami`, `ghcr:umami-software/umami:3.0.3`, an image on the host) | Checks the registry has it, looks up the tag's digest and runs the app pinned to it, so a moved tag never changes a running app behind its back. |
| A git repository | Fetches it on the host with hardened git, builds it in a fresh sandbox in the org ([Builds](../guides/builds.md)), and runs the image from the org's registry. |
| A database engine (Postgres, MySQL, MariaDB, MongoDB, Redis) | Runs the engine's official image with a data volume, a health check and generated credentials ([Databases](../guides/databases.md)). |
| A template | Creates one or more of the above with settings filled in and secrets generated ([Templates](../guides/templates.md)). |

## Deployments

Every deploy is a record with an id, what triggered it (the CLI, a tool call,
a webhook), who, the commit or image digest, its status and its log:

```text
queued → building → deploying → done
   │         └──────────┴──────→ failed
   └→ superseded
```

One deploy runs at a time per app; a newer request replaces a waiting one,
so a burst of pushes builds the latest commit once. **Rollback** puts back
what an earlier deployment ran (its image, by digest, and the settings it was
deployed with) without building. Pushes to a git host's webhook deploy the
app, and pull requests can get their own [preview
deployments](../guides/previews.md).

The full settings, the environment editor, git sources and webhooks are in
[Deploy apps](../guides/deploy-apps.md).

```console
$ isb project create shop
$ isb app create web --project shop --image docker:traefik/whoami \
    --port 80 -p 127.0.0.1:8080:80 --deploy
$ isb app deployments web
$ isb app rollback web
```
