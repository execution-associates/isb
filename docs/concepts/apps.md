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
- An environment also holds **compose stacks**: stacks deployed from a
  compose file you write ([below](#compose-stacks-in-an-environment)).

## Compose stacks in an environment

Every compose stack belongs to exactly one project environment, as in
Dokploy. `stack_deploy` (or `isb stack deploy --project P --env E`) puts a new
stack where you say, making the project if it does not exist; with neither,
it goes to the project named like the stack (its `production` environment,
else its first), made with a `production` environment when there is none. A
stack's owner never changes: to move one, remove it and deploy it again.

Belonging is a record on the project and nothing more. The stack keeps its
name, its services keep their names (`<service>.<stack>`), and nothing is
redeployed; what it gains is the environment's names,
`<service>.<project>-<env>`, so its services and the environment's apps reach
each other the same way. A name in an environment has one holder: a deploy
or an app that would take one already held is refused
([Service discovery](stacks.md#service-discovery) has the rule for names
held twice from before).

A stack with no owner when the daemon starts is adopted the same way, a
stack whose name does not fit a project name going to one made from its first
19 characters (numbered `-2`, `-3`, ... when that is taken). Removing a stack
(`isb stack rm`) takes it out of its environment. A project or environment
with compose stacks cannot be deleted, and an environment cannot be named so
that `<project>-<env>` is a compose stack's name.

## Sources

| Source | What isb does |
|---|---|
| An image (`docker:nginx:1.27`, `ghcr:org/app:tag`, a local alias) | Looks up the tag's digest and runs the app pinned to it, so a moved tag never changes a running app behind its back. |
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
