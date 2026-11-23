# @execution-associates/isb-alchemy

An [Alchemy](https://alchemy.run) provider for [isb](https://github.com/execution-associates/isb):
declare the apps, stacks and secrets an isb host runs in TypeScript, and
`alchemy deploy` makes the host match.

```sh
bun add @execution-associates/isb-alchemy alchemy effect
```

It needs alchemy 2 (the Effect-based releases, `2.0.0-beta.87` or later), an
isb server at 4.3 or later and an API token (`isb token create`, or the web UI) of a member
of the org.

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

```sh
export ISB_URL=https://isb.example.com ISB_TOKEN=isb_tok_... ISB_ORG=acme
bun alchemy deploy        # plan, ask, apply; --yes in CI
bun alchemy destroy
```

`Isb.providers()` reads the connection from `ISB_URL`, `ISB_TOKEN` and
`ISB_ORG` (default `default`), the variables the isb CLI reads. Pass it in
code instead with `Isb.providers({ url, token, org })`.

## Resources

| Resource | What it is | Destroyed |
|---|---|---|
| `Isb.Project` | A project and its environments. | Deleted once it holds no apps and no stacks. An environment dropped from `environments` stays. |
| `Isb.Secret` | A value in the org's secret store. `ref` is what an app's `env` takes. | Deleted; refused by isb while a deployed stack still uses it. |
| `Isb.App` | An app: an image, or a git repository isb builds, as a health-checked service with rolling updates, domains, volumes and secrets. | Its service is removed. Named volumes are kept. |
| `Isb.Stack` | A compose file run as an isb stack. | Removed, with its named volumes only when `removeVolumes: true`. |

`Isb.App` takes an app definition as `isb app export` prints it, as an
object: `name`, `project`, `environment`, `source`, `env`, `domains`,
`volumes`, `ports`, `replicas`, `port`, `healthcheck`, `resources`,
`command`, and any other field of a definition.

## What a deploy does

- **`Isb.App`** applies the whole definition, deploys it and waits for the
  service to converge. A failed deployment fails the deploy, with isb's
  reason and the end of the deployment log.
- **The definition is the whole desired state.** A domain, variable, volume
  or port left out of it is removed from the app.
- **Deploying the same thing again restarts nothing.** isb compares the
  rendered settings and the image's digest with what runs, and leaves the
  instances alone when both match. A tag that moved on its registry
  (`docker:nginx:latest`) is a change, and rolls out. `redeploy: "always"`
  on an app replaces its instances on every deploy instead. This needs
  isb 4.3 or later; an older one refuses the call.
- **A changed secret value** is a new version in isb, and the services
  that use the secret roll. The value is compared by a hash kept in
  alchemy's state; the same value writes nothing.
- **`name`, `project` and `environment` cannot change** in isb, so
  changing one replaces the resource: the old app is deleted, then the new
  one is created.

## Secrets and state

An `Isb.Secret`'s value is a prop, so alchemy keeps it in its state store.
Use a state store that encrypts at rest, or keep the value out of alchemy:
create the secret in isb yourself (`isb secret set`) and name it in the
app's `env` as `{ secret: "NAME" }`.

## Stages

Alchemy's stage (`--stage`, default `live_$USER`) names a copy of the
stack. isb's names are yours to choose, so put the stage where you want
copies to differ: as the project's environment, and in the names of apps
and secrets, which are unique within an org.

```ts
const stage = yield* Alchemy.Stage;
const shop = yield* Isb.Project("shop", { name: "shop", environments: [stage] });
yield* Isb.App("web", { name: `web-${stage}`, project: shop.name, environment: stage, ... });
```

## Development

```sh
bun install
bun run typecheck && bun run lint && bun test
```

The tests run the provider through alchemy's own engine against an
in-memory stand-in for isb's REST tools (`test/fake.ts`).
