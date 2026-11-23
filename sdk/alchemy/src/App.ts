import * as Provider from "alchemy/Provider";
import { Resource } from "alchemy/Resource";
import * as Effect from "effect/Effect";
import { call, callFound, IsbError } from "./Client.ts";
import { replaceWhen } from "./Replace.ts";

/** Where an app's image comes from. */
export type AppSource =
  | { image: string }
  | {
      git: {
        url: string;
        ref?: string;
        subdir?: string;
        auth?: { token_secret?: string; ssh_key_secret?: string };
      };
    }
  | { database: Record<string, unknown> };

/** A plain value, or a secret in the org's store (`Isb.Secret`'s `ref`). */
export type EnvValue = string | { secret: string };

/**
 * An app as isb's `app_apply` takes it (the YAML of `isb app export`, as
 * an object). The fields listed are the common ones; any other field of
 * an app definition passes through as written.
 */
export interface AppProps {
  /** The app's name: its service, and how other apps reach it. */
  name: string;
  project: string;
  /** @default "production" */
  environment?: string;
  source: AppSource;
  build?: { builder: Record<string, unknown>; args?: Record<string, string> };
  env?: Record<string, EnvValue>;
  domains?: ({ host: string; path?: string; port?: number } & Record<string, unknown>)[];
  /** `NAME:/path` named volumes. */
  volumes?: string[];
  /** `[IP:]PUBLISHED:TARGET` host ports. */
  ports?: string[];
  replicas?: number;
  /** The port the app listens on, for domains and the health check. */
  port?: number;
  healthcheck?: Record<string, unknown>;
  resources?: { cpus?: string; memory?: string };
  command?: string | string[];
  /**
   * What a deploy does to instances that already run exactly this:
   * `"if-changed"` leaves them running (the settings and the image digest
   * are the same), `"always"` replaces them on every deploy.
   * @default "if-changed"
   */
  redeploy?: "if-changed" | "always";
  /**
   * How long a deployment may take before the deploy fails, in seconds.
   * @default 900
   */
  timeoutSeconds?: number;
  [field: string]: unknown;
}

export interface AppAttributes {
  name: string;
  project: string;
  environment: string;
  /** The stack the app's service runs in: `<project>-<environment>`. */
  stack: string;
  /** The name other services in the org reach it by. */
  serviceName: string;
  /** The deployment that last ran, and what it deployed. */
  deployment: number;
  image: string | undefined;
  digest: string | undefined;
  /** The last deployment changed nothing, and the instances were left running. */
  unchanged: boolean;
  /** `https://<host><path>` for each of the app's domains. */
  urls: string[];
}

export type App = Resource<"Isb.App", AppProps, AppAttributes>;

interface DeploymentRecord {
  id: number;
  status: string;
  image?: string;
  digest?: string;
  error?: string;
  unchanged?: boolean;
}

interface AppRecord {
  name: string;
  project: string;
  environment: string;
  stack: string;
  service_name: string;
  domains?: { host: string; path?: string }[];
}

/** Follow a deployment's log until it finishes; its record and the log. */
const follow = (name: string, id: number, timeoutSeconds: number) =>
  Effect.gen(function* () {
    let offset = 0;
    let log = "";
    const deadline = Date.now() + timeoutSeconds * 1000;
    while (true) {
      const page = yield* call<{
        log: string;
        offset: number;
        finished: boolean;
        deployment: DeploymentRecord;
      }>("app_deployment_log", { name, deployment: id, offset });
      log += page.log;
      offset = page.offset;
      if (page.finished) return { record: page.deployment, log };
      if (Date.now() > deadline) {
        return yield* Effect.fail(
          new IsbError({
            tool: "app_deployment_log",
            code: "timeout",
            status: 0,
            message: `isb: deployment ${id} of app ${name} did not finish within ${timeoutSeconds}s`,
          }),
        );
      }
      yield* Effect.sleep("1 second");
    }
  });

/** The last lines of a deployment log, for an error message. */
const tail = (log: string, lines = 15) => log.trimEnd().split("\n").slice(-lines).join("\n");

/**
 * An app on isb: an image (or a git repository isb builds), run as a
 * health-checked service with rolling updates, domains, volumes and
 * secrets.
 *
 * Each deploy applies the whole definition (`app_apply`), deploys it and
 * waits for the service to converge. A deploy that changes nothing leaves
 * the running instances alone: isb compares the settings and the image's
 * digest, so a tag that moved on its registry still rolls out.
 *
 * ```typescript
 * const web = yield* Isb.App("web", {
 *   name: "web",
 *   project: shop.name,
 *   source: { image: "ghcr:acme/shop:1.4.0" },
 *   port: 8080,
 *   replicas: 2,
 *   domains: [{ host: "shop.example.com" }],
 *   env: { LOG_LEVEL: "info", DATABASE_URL: dbUrl.ref },
 * });
 * ```
 *
 * The definition is the app's whole desired state: a domain, variable,
 * volume or port left out of it is removed from the app. An app's `name`,
 * `project` and `environment` cannot change, so changing one replaces the
 * app. Destroying it keeps its named volumes.
 */
export const App = Resource<App>("Isb.App");

export const AppProvider = () =>
  Provider.succeed(App, {
    stables: ["name", "project", "environment", "stack", "serviceName"],
    // An app's name, project and environment cannot change. The old app
    // goes first: the replacement takes its service name.
    diff: replaceWhen<AppProps>((p) => [p.name, p.project, p.environment ?? "production"], {
      deleteFirst: true,
    }),
    reconcile: Effect.fn(function* ({ news }) {
      const { redeploy = "if-changed", timeoutSeconds = 900, ...definition } = news;
      const applied = yield* call<{ deployment: { id: number } }>("app_apply", {
        definition,
        deploy: true,
        if_changed: redeploy === "if-changed",
        // The definition here is the source of truth, removals included.
        allow_removals: true,
      });
      const { record, log } = yield* follow(news.name, applied.deployment.id, timeoutSeconds);
      if (record.status !== "done") {
        return yield* Effect.fail(
          new IsbError({
            tool: "app_deploy",
            code: "deploy_failed",
            status: 0,
            message: `isb: deployment ${record.id} of app ${news.name} ${record.status}: ${
              record.error ?? "no reason given"
            }\n${tail(log)}`,
          }),
        );
      }
      const app = yield* call<AppRecord>("app_get", { name: news.name });
      return {
        name: app.name,
        project: app.project,
        environment: app.environment,
        stack: app.stack,
        serviceName: app.service_name,
        deployment: record.id,
        image: record.image,
        digest: record.digest,
        unchanged: record.unchanged ?? false,
        urls: (app.domains ?? []).map((d) => `https://${d.host}${d.path ?? ""}`),
      };
    }),
    delete: Effect.fn(function* ({ output }) {
      yield* callFound("app_delete", { name: output.name });
    }),
  });
