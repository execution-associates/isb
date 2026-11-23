/**
 * A project with one app and its secret, deployed to isb.
 *
 *   export ISB_URL=https://isb.example.com ISB_TOKEN=isb_tok_... ISB_ORG=acme
 *   bun alchemy deploy --stage production
 *   bun alchemy destroy --stage production
 *
 * The stage is the isb environment the app runs in, so `--stage staging`
 * deploys a second copy as the stack `shop-staging`.
 */
import * as Isb from "@execution-associates/isb-alchemy";
import * as Alchemy from "alchemy";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";

export default Alchemy.Stack(
  "shop",
  { providers: Isb.providers(), state: Alchemy.localState() },
  Effect.gen(function* () {
    const stage = yield* Alchemy.Stage;

    const shop = yield* Isb.Project("shop", { name: "shop", environments: [stage] });

    const greeting = yield* Isb.Secret("greeting", {
      name: `shop-${stage}-greeting`,
      value: Redacted.make(process.env.GREETING ?? "hello"),
    });

    const web = yield* Isb.App("web", {
      name: `web-${stage}`,
      project: shop.name,
      environment: stage,
      source: { image: "docker:traefik/whoami:v1.11" },
      port: 80,
      replicas: 2,
      env: { LOG_LEVEL: "info", GREETING: greeting.ref },
      // domains: [{ host: "shop.example.com" }],
    });

    return { service: web.serviceName, image: web.image, deployment: web.deployment };
  }),
);
