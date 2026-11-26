/**
 * An in-memory stand-in for an isb server's REST tools, with the semantics
 * the provider leans on: apply is idempotent, a deploy with `if_changed`
 * leaves an unchanged app alone, and an unknown thing is `not_found`.
 */

// biome-ignore lint/suspicious/noExplicitAny: tool arguments are loose JSON
type Args = Record<string, any>;

export interface FakeIsb {
  url: string;
  /** Every call, in order: `[tool, args]`. */
  calls: [string, Args][];
  apps: Map<string, { definition: Args; deployed?: string; restarts: number }>;
  secrets: Map<string, { value: string; version: number }>;
  projects: Map<string, string[]>;
  stacks: Map<string, { compose: string; revision: number }>;
  /** Fail the next deployment of an app with this message. */
  failNextDeploy?: string;
  stop(): void;
}

class Refusal extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

/**
 * `access`, when given, puts the fake behind Cloudflare Access: a request
 * without that service token's headers is refused before isb sees its token.
 */
export const startFakeIsb = (
  token: string,
  access?: { clientId: string; clientSecret: string },
): FakeIsb => {
  const deployments = new Map<string, Args[]>();
  const fake: FakeIsb = {
    url: "",
    calls: [],
    apps: new Map(),
    secrets: new Map(),
    projects: new Map(),
    stacks: new Map(),
    stop: () => server.stop(true),
  };

  const tools: Record<string, (a: Args) => unknown> = {
    project_list: () => ({
      projects: [...fake.projects].map(([name, envs]) => ({
        name,
        environments: envs.map((e) => ({ name: e, stack: `${name}-${e}`, apps: [] })),
      })),
    }),
    project_create: (a) => {
      if (fake.projects.has(a.name)) throw new Refusal("invalid", `project ${a.name} exists`);
      fake.projects.set(a.name, [...(a.environments ?? ["production"])]);
      return {};
    },
    environment_create: (a) => {
      const envs = fake.projects.get(a.project);
      if (!envs) throw new Refusal("not_found", `project ${a.project}`);
      envs.push(a.name);
      return {};
    },
    project_delete: (a) => {
      if (!fake.projects.delete(a.name)) throw new Refusal("not_found", `project ${a.name}`);
      return {};
    },
    secret_inspect: (a) => {
      const s = fake.secrets.get(a.name);
      if (!s) throw new Refusal("not_found", `secret ${a.name}`);
      return { name: a.name, version: s.version };
    },
    secret_set: (a) => {
      const version = (fake.secrets.get(a.name)?.version ?? 0) + 1;
      fake.secrets.set(a.name, { value: atob(a.value), version });
      return { version };
    },
    secret_delete: (a) => {
      if (!fake.secrets.delete(a.name)) throw new Refusal("not_found", `secret ${a.name}`);
      return {};
    },
    app_apply: (a) => {
      const d = a.definition as Args;
      if (!fake.projects.has(d.project)) throw new Refusal("invalid", `no project ${d.project}`);
      const app = fake.apps.get(d.name) ?? { definition: d, restarts: 0 };
      app.definition = d;
      fake.apps.set(d.name, app);
      const rendered = JSON.stringify(d);
      const record: Args = { id: (deployments.get(d.name)?.length ?? 0) + 1, status: "done" };
      if (fake.failNextDeploy) {
        record.status = "failed";
        record.error = fake.failNextDeploy;
        fake.failNextDeploy = undefined;
      } else if (a.if_changed && app.deployed === rendered) {
        record.unchanged = true;
      } else {
        app.restarts += 1;
        app.deployed = rendered;
      }
      record.image = d.source?.image;
      deployments.set(d.name, [...(deployments.get(d.name) ?? []), record]);
      return { action: "updated", deployment: { id: record.id, status: "queued" } };
    },
    app_deployment_log: (a) => {
      const record = deployments.get(a.name)?.[a.deployment - 1];
      if (!record) throw new Refusal("not_found", `deployment ${a.deployment}`);
      const log = "image resolved\nservice converged\n";
      return {
        log: log.slice(a.offset),
        offset: log.length,
        finished: true,
        status: record.status,
        deployment: record,
      };
    },
    app_get: (a) => {
      const app = fake.apps.get(a.name);
      if (!app) throw new Refusal("not_found", `app ${a.name}`);
      const d = app.definition;
      const environment = d.environment ?? "production";
      return {
        ...d,
        environment,
        stack: `${d.project}-${environment}`,
        service_name: `${d.name}.${d.project}-${environment}`,
      };
    },
    app_delete: (a) => {
      if (!fake.apps.delete(a.name)) throw new Refusal("not_found", `app ${a.name}`);
      return {};
    },
    stack_deploy: (a) => {
      const old = fake.stacks.get(a.name);
      const changed = old?.compose !== a.compose;
      fake.stacks.set(a.name, {
        compose: a.compose,
        revision: (old?.revision ?? 0) + (changed ? 1 : 0),
      });
      return { changes: [] };
    },
    stack_status: (a) => {
      const s = fake.stacks.get(a.name);
      if (!s) throw new Refusal("not_found", `stack ${a.name}`);
      return {
        converged: true,
        services: [{ service: "web", rev: `r${s.revision}`, state: "converged" }],
      };
    },
    stack_remove: (a) => {
      if (!fake.stacks.delete(a.name)) throw new Refusal("not_found", `stack ${a.name}`);
      return {};
    },
  };

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    async fetch(req) {
      const m = new URL(req.url).pathname.match(/^\/orgs\/acme\/api\/v1\/tools\/([a-z_]+)$/);
      const json = (status: number, body: unknown) => Response.json(body, { status });
      if (
        access &&
        (req.headers.get("cf-access-client-id") !== access.clientId ||
          req.headers.get("cf-access-client-secret") !== access.clientSecret)
      ) {
        return json(401, {
          error: "unauthorized",
          message: "missing Cloudflare Access assertion",
        });
      }
      if (req.headers.get("authorization") !== `Bearer ${token}`) {
        return json(401, { error: "unauthorized", message: "bad token" });
      }
      const tool = m?.[1];
      const run = tool ? tools[tool] : undefined;
      if (!tool || !run) return json(404, { error: "not_found", message: "no such tool" });
      const args = (await req.json()) as Args;
      fake.calls.push([tool, args]);
      try {
        return json(200, { result: run(args) });
      } catch (e) {
        if (!(e instanceof Refusal)) throw e;
        return json(e.code === "not_found" ? 404 : 400, { error: e.code, message: e.message });
      }
    },
  });
  fake.url = `http://127.0.0.1:${server.port}`;
  return fake;
};
