---
title: Deploy with Alchemy
nav_title: Alchemy
description: Declare an isb host's projects, apps, stacks and secrets in TypeScript with the Alchemy provider, and deploy them with alchemy deploy.
order: 18.5
---

[Alchemy](https://alchemy.run) is infrastructure-as-code in TypeScript: a
stack file declares resources, and `alchemy deploy` plans and applies the
difference, keeping what it made in a state store. The isb provider,
`@execution-associates/isb-alchemy`, adds isb's projects, secrets, apps and
stacks as resources, so an app on isb can sit in the same stack as its DNS
record, its database or its bucket somewhere else.

It talks to an isb server over [the REST API](../reference/http-api.md#rest-tools)
with an API token, from anywhere that reaches it: a laptop, or CI.

## Set up

```sh
bun add @execution-associates/isb-alchemy alchemy effect
isb token create ci --org acme         # or the web UI: the token is shown once
export ISB_URL=https://isb.example.com ISB_ORG=acme ISB_TOKEN=isb_tok_...
```

The provider needs alchemy 2 (`2.0.0-beta.87` or later) and an isb server
at 4.3 or later, and reads
`ISB_URL`, `ISB_TOKEN` and `ISB_ORG` (default `default`). The token's user
must be a member of the org. The `deploy` scope is too narrow (it has no
`app_apply`): leave the token unscoped, or scope it by tool to what the
stack's resources call, `--scope 'tool:app_*' --scope 'tool:project_*'
--scope 'tool:environment_*' --scope 'tool:secret_*' --scope 'tool:stack_*'`.

## A stack

```ts
// alchemy.run.ts
import * as Isb from "@execution-associates/isb-alchemy";
import * as Alchemy from "alchemy";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";

export default Alchemy.Stack(
  "shop",
  { providers: Isb.providers(), state: Alchemy.localState() },
  Effect.gen(function* () {
    const shop = yield* Isb.Project("shop", { name: "shop" });

    const dbUrl = yield* Isb.Secret("db-url", {
      name: "shop-db-url",
      value: Redacted.make(process.env.DATABASE_URL!),
    });

    const web = yield* Isb.App("web", {
      name: "web",
      project: shop.name,
      source: { image: "ghcr:acme/shop:1.4.0" },
      port: 8080,
      replicas: 2,
      domains: [{ host: "shop.example.com" }],
      env: { LOG_LEVEL: "info", DATABASE_URL: dbUrl.ref },
    });

    return { urls: web.urls, service: web.serviceName };
  }),
);
```

```console
$ bun alchemy deploy        # plans, asks, applies; --yes where nobody can answer
$ bun alchemy plan          # only the plan
$ bun alchemy destroy
```

## Resources

| Resource | Props | Outputs |
|---|---|---|
| `Isb.Project` | `name`, `description`, `environments` (default `["production"]`) | `name`, `environments` |
| `Isb.Secret` | `name`, `value` (a `Redacted`) | `name`, `version`, `ref` (what an app's `env` takes) |
| `Isb.App` | an [app definition](deploy-apps.md#an-app-as-yaml) as an object, plus `redeploy` and `timeoutSeconds` | `name`, `stack`, `serviceName`, `deployment`, `image`, `digest`, `unchanged`, `urls` |
| `Isb.Stack` | `name`, `compose` (YAML text), `vars`, `project`, `environment`, `removeVolumes`, `timeoutSeconds` | `name`, `revisions` |

## What a deploy does

`Isb.App` calls `app_apply` with the whole definition and `deploy`, then
follows the deployment until the service converges. A deployment that fails
fails the deploy, with isb's reason and the end of its log.

- The definition is the app's whole desired state: a domain, variable,
  volume or port left out of it is removed.
- Deploying the same definition again restarts nothing. The provider
  deploys with [`if_changed`](deploy-apps.md#deployments), so isb leaves
  the instances running when the settings and the image digest are what
  already runs. A tag that moved on its registry is a change and rolls out.
  `redeploy: "always"` replaces the instances on every deploy.
- A new secret value is a new version, and the services using it roll.
  The same value writes nothing.
- An app's `name`, `project` and `environment`, and a stack's or secret's
  `name`, cannot change in isb: changing one replaces the resource.
- Destroying an app or a stack keeps its named volumes (`removeVolumes:
  true` on a stack deletes them).

`Isb.Stack` deploys a compose file as [a stack](../concepts/stacks.md) and
waits for every service. Only the services whose settings or image digest
changed are replaced.

## Secrets and state

A secret's value is a prop, and alchemy keeps props in its state store. Use
a store that encrypts at rest, or keep the value out of alchemy: set the
secret in isb (`isb secret set shop-db-url`) and name it in the app as
`env: { DATABASE_URL: { secret: "shop-db-url" } }`.

## Stages

`alchemy deploy --stage NAME` deploys a named copy of the stack. Names in
isb are yours to choose, so put the stage where copies should differ: the
project's environment, and the names of apps and secrets, which are unique
within an org.

```ts
const stage = yield* Alchemy.Stage;
const shop = yield* Isb.Project("shop", { name: "shop", environments: [stage] });
yield* Isb.App("web", {
  name: `web-${stage}`,
  project: shop.name,
  environment: stage,
  source: { image: "ghcr:acme/shop:1.4.0" },
});
```
