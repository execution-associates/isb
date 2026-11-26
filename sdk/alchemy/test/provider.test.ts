import { afterAll, expect } from "bun:test";
import * as Test from "@alchemy.run/test/Bun";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";
import * as Isb from "../src/index.ts";
import { startFakeIsb } from "./fake.ts";

// The fake sits behind Access, so every call in this file also proves the
// provider sends the service token's headers.
const access = { clientId: "deploy.access", clientSecret: "access-secret" };
const fake = startFakeIsb("isb_tok_test", access);
afterAll(() => fake.stop());

const { test } = Test.make({
  providers: Isb.providers({ url: fake.url, org: "acme", token: "isb_tok_test", access }),
});

const called = (tool: string) => fake.calls.filter(([t]) => t === tool).length;

const shop = (opts: { image: string; token: string; envs?: string[] }) =>
  Effect.gen(function* () {
    const project = yield* Isb.Project("shop", {
      name: "shop",
      environments: opts.envs ?? ["production"],
    });
    const secret = yield* Isb.Secret("token", {
      name: "shop-token",
      value: Redacted.make(opts.token),
    });
    const web = yield* Isb.App("web", {
      name: "web",
      project: project.name,
      source: { image: opts.image },
      port: 80,
      env: { LOG_LEVEL: "info", TOKEN: secret.ref },
    });
    return { url: web.serviceName, deployment: web.deployment, unchanged: web.unchanged };
  });

test.provider("a project, a secret and an app: create, redeploy, update, destroy", (stack) =>
  Effect.gen(function* () {
    const first = yield* stack.deploy(shop({ image: "docker:nginx:1.27", token: "one" }));
    expect(first.url).toBe("web.shop-production");
    expect(first.unchanged).toBe(false);
    expect(fake.projects.get("shop")).toEqual(["production"]);
    expect(fake.secrets.get("shop-token")).toEqual({ value: "one", version: 1 });
    const app = fake.apps.get("web");
    expect(app?.restarts).toBe(1);
    // The secret reaches the app by reference, never by value.
    expect(app?.definition.env).toEqual({ LOG_LEVEL: "info", TOKEN: { secret: "shop-token" } });
    // The provider's own knobs are not part of the definition isb gets.
    expect(app?.definition.redeploy).toBeUndefined();
    const apply = fake.calls.find(([t]) => t === "app_apply")?.[1];
    expect(apply).toMatchObject({ deploy: true, if_changed: true, allow_removals: true });

    // The same stack again: nothing is written and nothing restarts.
    const sets = called("secret_set");
    yield* stack.deploy(shop({ image: "docker:nginx:1.27", token: "one" }));
    expect(called("secret_set")).toBe(sets);
    expect(fake.apps.get("web")?.restarts).toBe(1);

    // A new image rolls the app; a new secret value is a new version; a
    // new environment is added to the project.
    const second = yield* stack.deploy(
      shop({ image: "docker:nginx:1.28", token: "two", envs: ["production", "staging"] }),
    );
    expect(second.unchanged).toBe(false);
    expect(fake.apps.get("web")?.restarts).toBe(2);
    expect(fake.secrets.get("shop-token")).toEqual({ value: "two", version: 2 });
    expect(fake.projects.get("shop")).toEqual(["production", "staging"]);

    yield* stack.destroy();
    expect(fake.apps.size).toBe(0);
    expect(fake.secrets.size).toBe(0);
    expect(fake.projects.size).toBe(0);
  }),
);

test.provider("a failed deployment fails the deploy with isb's reason and log", (stack) =>
  Effect.gen(function* () {
    fake.failNextDeploy = "image docker:nope not found on Docker Hub (manifest unknown)";
    const exit = yield* Effect.exit(stack.deploy(shop({ image: "docker:nope", token: "one" })));
    expect(exit._tag).toBe("Failure");
    const text = JSON.stringify(exit);
    expect(text).toMatch(/deployment \d+ of app web failed/);
    expect(text).toContain("manifest unknown");
    yield* stack.destroy();
  }),
);

test.provider("a compose stack: deploy, redeploy, change, destroy", (stack) =>
  Effect.gen(function* () {
    const wiki = (tag: string) =>
      Isb.Stack("wiki", {
        name: "wiki",
        compose: `services:\n  web:\n    image: docker:nginx:${tag}\n`,
      }).pipe(Effect.map((s) => ({ revisions: s.revisions })));
    const first = yield* stack.deploy(wiki("1.27"));
    expect(first.revisions).toEqual({ web: "r1" });
    const second = yield* stack.deploy(wiki("1.28"));
    expect(second.revisions).toEqual({ web: "r2" });
    yield* stack.destroy();
    expect(fake.stacks.size).toBe(0);
  }),
);
