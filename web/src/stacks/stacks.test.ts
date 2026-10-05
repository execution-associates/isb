import { describe, expect, it } from "vitest";
import type { Project } from "@/apps/api";
import { activeServiceTab, serviceTabs, stackTab } from "@/apps/service-tabs";
import { sameTarget, targetName } from "@/jobs/api";
import { elapsed } from "@/apps/deployments-tab";
import { toYaml } from "@/lib/yaml-edit";
import { stepStates } from "@/lib/status";
import {
  asDeployment,
  composePath,
  composeStacks,
  eventLine,
  eventServices,
  ownerOf,
  reusedSecrets,
  sourceReplicas,
  stackDeploymentDetail,
  stackDomains,
  stackRedirect,
  stackStages,
} from "./api";
import { reusedSecretsLine, stackDeploymentAction, stackDeploymentServices, stackRecentLabel, stackSource } from "./stack-deployments";

const project = (name: string, envs: Record<string, string[]>): Project => ({
  name,
  created_at: 0,
  environments: Object.entries(envs).map(([e, compose]) => ({ name: e, stack: `${name}-${e}`, apps: [], compose: compose.map((c) => ({ name: c, services: ["web"] })) })),
});

const projects = [project("shop", { production: ["monitoring"], staging: [] }), project("blog", { production: ["search", "cache"] })];

describe("composeStacks", () => {
  it("lists every environment's compose stacks with their owner", () => {
    expect(composeStacks(projects).map((s) => `${s.project}/${s.environment}/${s.name}`)).toEqual([
      "shop/production/monitoring",
      "blog/production/search",
      "blog/production/cache",
    ]);
  });
});

describe("ownerOf", () => {
  it("finds the project environment a compose stack is deployed into", () => {
    expect(ownerOf(projects, "cache")).toEqual({ project: "blog", environment: "production" });
  });

  it("is null for a project's apps stack, isb's tunnel, or an unknown stack", () => {
    expect(ownerOf(projects, "shop-production")).toBeNull();
    expect(ownerOf(projects, "isb-tunnel")).toBeNull();
    expect(ownerOf(projects, "nope")).toBeNull();
  });
});

describe("stackRedirect", () => {
  it("sends an old stack link to its page under the project environment, keeping the tab", () => {
    expect(stackRedirect("acme", "monitoring", undefined, projects)).toBe("/orgs/acme/projects/shop/production/compose/monitoring");
    expect(stackRedirect("acme", "search", "logs", projects)).toBe("/orgs/acme/projects/blog/production/compose/search/logs");
  });

  it("falls back to the export's owner when the projects don't list the stack yet", () => {
    expect(stackRedirect("acme", "fresh", "logs", projects, { managed_by: null, project: "shop", environment: "staging" })).toBe(
      "/orgs/acme/projects/shop/staging/compose/fresh/logs",
    );
  });

  it("stays put for a stack no project owns as compose", () => {
    expect(stackRedirect("acme", "isb-tunnel", undefined, projects, { managed_by: "ingress", project: null, environment: null })).toBeNull();
    expect(stackRedirect("acme", "shop-production", undefined, projects, { managed_by: "apps", project: "shop", environment: "production" })).toBeNull();
    expect(stackRedirect("acme", "gone", undefined, projects)).toBeNull();
  });

  it("encodes names in the path", () => {
    expect(composePath("my org", { project: "shop", environment: "production" }, "web", "compose")).toBe("/orgs/my%20org/projects/shop/production/compose/web/compose");
  });
});

describe("old compose tab links", () => {
  it("map Compose to YAML and Services to General, leaving the rest", () => {
    expect(stackTab("compose")).toBe("yaml");
    expect(stackTab("services")).toBe("general");
    expect(stackTab("logs")).toBe("logs");
    expect(stackTab("constructor")).toBe("constructor");
    expect(stackTab(undefined)).toBeUndefined();
  });

  it("redirect an old stack link to the new tab name", () => {
    expect(stackRedirect("acme", "search", "compose", projects)).toBe("/orgs/acme/projects/blog/production/compose/search/yaml");
    expect(stackRedirect("acme", "search", "services", projects)).toBe("/orgs/acme/projects/blog/production/compose/search/general");
    expect(stackRedirect("acme", "search", "deployments/7", projects)).toBe("/orgs/acme/projects/blog/production/compose/search/deployments/7");
  });
});

describe("a compose stack's tabs", () => {
  it("are an app's, in the same order, without the database and git ones", () => {
    const app = serviceTabs({ writer: true, git: true }).map((t) => t.id);
    const compose = serviceTabs({ writer: true }).map((t) => t.id);
    expect(compose).toEqual(["general", "domains", "environment", "deployments", "logs", "monitoring", "terminal", "jobs", "yaml", "advanced"]);
    expect(app.filter((t) => t !== "previews")).toEqual(compose);
  });

  it("open on General, and on YAML only when asked", () => {
    const tabs = serviceTabs({ writer: true });
    expect(activeServiceTab(undefined, tabs)).toBe("general");
    expect(activeServiceTab(stackTab("compose"), tabs)).toBe("yaml");
    expect(activeServiceTab(stackTab("services"), tabs)).toBe("general");
  });
});

describe("compose day-2 adapters", () => {
  it("read stack_domains_get, filling missing lists", () => {
    expect(stackDomains({ services: { web: { managed: [{ host: "a.example.com" }] }, db: null } })).toEqual({
      web: { managed: [{ host: "a.example.com" }], file: [] },
      db: { managed: [], file: [] },
    });
    expect(stackDomains(undefined)).toEqual({});
  });

  it("take a deployment flat or wrapped, in seconds or milliseconds", () => {
    const flat = stackDeploymentDetail({ id: 3, trigger: "api", status: "done", created_at: 1_700_000_000, source: "services: {}\n", events: ["queued", { service: "web", message: "rolled" }], log: "a\nb\n" });
    expect(flat.record.created_at).toBe(1_700_000_000_000);
    expect(flat.source).toBe("services: {}\n");
    // The log is the events as text: the events win, the log stands in without them.
    expect(flat.lines).toEqual(["queued", "web: rolled"]);
    expect(stackDeploymentDetail({ id: 5, trigger: "api", status: "done", created_at: 1, log: "a\nb\n" }).lines).toEqual(["a", "b"]);
    const wrapped = stackDeploymentDetail({ deployment: { id: 4, trigger: "webhook", status: "failed", created_at: 1_700_000_000_123 } });
    expect(wrapped.record.id).toBe(4);
    expect(wrapped.record.created_at).toBe(1_700_000_000_123);
    expect(wrapped.lines).toEqual([]);
  });

  it("draw a stack deployment as an app deployment", () => {
    const d = asDeployment("search", { id: 9, trigger: "api", status: "done", actor: "ada", created_at: 1_700_000_000, finished_at: 1_700_000_030, rollback_of: 7 });
    expect(d).toMatchObject({ id: 9, app: "search", by: "ada", status: "done", rollback_of: 7, created_at: 1_700_000_000_000, finished_at: 1_700_000_030_000 });
    expect(asDeployment("search", { id: 1, trigger: "api", status: "queued", created_at: 1 }).by).toBe("someone");
  });

  it("match jobs to an app or a stack service", () => {
    expect(sameTarget({ stack: "search", service: "web" }, { stack: "search", service: "web" })).toBe(true);
    expect(sameTarget({ stack: "search", service: "web" }, { stack: "search", service: "worker" })).toBe(false);
    expect(sameTarget({ app: "web" }, { stack: "search", service: "web" })).toBe(false);
    expect(sameTarget({ app: "web" }, { app: "web" })).toBe(true);
    expect(targetName({ stack: "search", service: "web" })).toBe("web");
  });
});

describe("a stack deployment's row", () => {
  it("says how it came about", () => {
    expect(stackDeploymentAction({ action: "rollback", rollback_of: 1, trigger: "api" })).toBe("Rollback to #1");
    expect(stackDeploymentAction({ rollback_of: 4, trigger: "api" })).toBe("Rollback to #4");
    expect(stackDeploymentAction({ action: "env", trigger: "api" })).toBe("Environment changed");
    expect(stackDeploymentAction({ action: "domains", trigger: "manual" })).toBe("Domains changed");
    expect(stackDeploymentAction({ action: "deploy", trigger: "api" })).toBe("Manual");
    expect(stackDeploymentAction({ action: "deploy", trigger: "manual" })).toBe("CLI");
    expect(stackDeploymentAction({ trigger: "webhook" })).toBe("Webhook");
  });

  it("names the services it changed, and none as no changes once finished", () => {
    expect(stackDeploymentServices({ services: ["web", "worker"], status: "done" })).toBe("web, worker");
    expect(stackDeploymentServices({ services: [], status: "done" })).toBeNull();
    expect(stackDeploymentServices({ status: "failed" })).toBeNull();
    expect(stackDeploymentServices({ services: [], status: "deploying" })).toBe("");
  });

  it("has a duration whenever it finished at a known time", () => {
    // A rollback recorded in whole seconds, done within the second.
    const d = asDeployment("s", { id: 2, trigger: "api", status: "done", action: "rollback", rollback_of: 1, created_at: 1_700_000_000, finished_at: 1_700_000_000 });
    expect(elapsed(d)).toBe(0);
    const started = asDeployment("s", { id: 3, trigger: "api", status: "done", created_at: 1_700_000_000, started_at: 1_700_000_002, finished_at: 1_700_000_007 });
    expect(elapsed(started)).toBe(5000);
    expect(elapsed(asDeployment("s", { id: 4, trigger: "api", status: "done", created_at: 1_700_000_000 }))).toBeNull();
  });

  it("reads reused secrets from the answer or its deployment", () => {
    expect(reusedSecrets({ reused_secrets: ["a", "b"] })).toEqual(["a", "b"]);
    expect(reusedSecrets({ deployment: { id: 1, trigger: "api", status: "deploying", created_at: 0, reused_secrets: ["c"] } })).toEqual(["c"]);
    expect(reusedSecrets({ reused_secrets: [] })).toEqual([]);
    expect(reusedSecrets(undefined)).toEqual([]);
    expect(reusedSecretsLine(["a", "b"])).toBe("Secrets reused from an earlier deploy: a, b — no new value was given");
  });
});

describe("Start's replicas from the compose file", () => {
  it("reads deploy.replicas per service, block or flow style, 1 where unset", () => {
    const yaml = [
      "# a comment",
      "name: shop",
      "services:",
      "  web:",
      "    image: docker:nginx",
      "    ports: [\"127.0.0.1:8080:80\"]",
      "    deploy:",
      "      update_config:",
      "        replicas: 9 # not deploy.replicas",
      "      replicas: 3 # three",
      "  worker:",
      "    image: docker:busybox",
      "    deploy: { replicas: 2 }",
      "  cache:",
      "    image: docker:redis",
      "    environment:",
      "      replicas: 7",
      "  vars:",
      "    deploy:",
      "      replicas: ${N}",
      "volumes:",
      "  data:",
      "    deploy:",
      "      replicas: 5",
    ].join("\n");
    expect(sourceReplicas(yaml, ["web", "worker", "cache", "vars", "missing"])).toEqual({ web: 3, worker: 2, cache: 1, vars: 1, missing: 1 });
  });

  it("takes 4-space indents and a deploy block with replicas 0", () => {
    const yaml = "services:\n    api:\n        deploy:\n            replicas: 0\n    ui:\n        image: x\n";
    expect(sourceReplicas(yaml, ["api", "ui"])).toEqual({ api: 0, ui: 1 });
  });
});

describe("toYaml", () => {
  it("writes block YAML, quoting only what would read back as something else", () => {
    expect(
      toYaml({
        services: {
          web: {
            image: "docker:nginx:1.27",
            ports: ["127.0.0.1:8080:80"],
            environment: { MODE: "true", PORT: 8080, EMPTY: "", NOTE: "a: b" },
            command: ["sh", "-c", "echo hi"],
            domains: [{ host: "a.example.com", port: 80 }],
            healthcheck: null,
            script: "line one\nline two\n",
          },
        },
        volumes: {},
      }),
    ).toBe(
      [
        "services:",
        "  web:",
        "    image: docker:nginx:1.27",
        "    ports:",
        '      - "127.0.0.1:8080:80"',
        "    environment:",
        '      MODE: "true"',
        "      PORT: 8080",
        '      EMPTY: ""',
        '      NOTE: "a: b"',
        "    command:",
        "      - sh",
        '      - "-c"',
        "      - echo hi",
        "    domains:",
        "      - host: a.example.com",
        "        port: 80",
        "    script: |",
        "      line one",
        "      line two",
        "volumes: {}",
        "",
      ].join("\n"),
    );
  });
});

describe("a stack deployment on the deployment page", () => {
  const ev = (message: string, service?: string, level = "info") => ({ at: 1_791_172_387_000, level, service, message });
  // The events stack_deployment_get answers for a compose deploy.
  const rollout = [
    ev("deployed by dev@dev.com: web update"),
    ev("rolling out rev 005cf334 to 2 slot(s), stop-first", "web"),
    ev("slot 1: replacing wiki-web-1-2906 (stop-first)", "web"),
    ev("slot 1: creating wiki-web-1-4fe7 (rev 005cf334)", "web"),
  ];

  it("is at Pull until a slot rolls out, then at Roll out", () => {
    const before = stackStages({ status: "deploying", started_at: 1 }, rollout.slice(0, 1));
    expect(before).toEqual({ status: "building", reached: { building: true, deploying: false } });
    expect(stepStates(before.status, before.reached)).toMatchObject({ queued: "done", building: "current", deploying: "waiting" });
    const during = stackStages({ status: "deploying", started_at: 1 }, rollout);
    expect(during).toEqual({ status: "deploying", reached: { building: true, deploying: true } });
    expect(stepStates(during.status, during.reached)).toMatchObject({ queued: "done", building: "done", deploying: "current", done: "waiting" });
  });

  it("pins a failure to the stage it happened in", () => {
    const rolling = stackStages({ status: "failed", started_at: 1 }, [...rollout, ev("slot 1: wiki-web-1-4fe7 failed its health check", "web", "error")]);
    expect(stepStates(rolling.status, rolling.reached)).toMatchObject({ building: "done", deploying: "failed", done: "skipped" });
    const early = stackStages({ status: "failed", started_at: 1 }, [ev("cannot resolve image", undefined, "error")]);
    expect(stepStates(early.status, early.reached)).toMatchObject({ building: "failed", deploying: "skipped" });
    const never = stackStages({ status: "failed" }, []);
    expect(stepStates(never.status, never.reached)).toMatchObject({ queued: "failed", building: "skipped" });
  });

  it("draws done and superseded as an app deployment's", () => {
    const done = stackStages({ status: "done", started_at: 1 }, []);
    expect(Object.values(stepStates(done.status, done.reached))).toEqual(["done", "done", "done", "done"]);
    const sup = stackStages({ status: "superseded" }, []);
    expect(stepStates(sup.status, sup.reached)).toMatchObject({ queued: "failed", building: "skipped" });
  });

  it("writes events as log lines, and lists their services", () => {
    expect(eventLine({ message: "slot 1: creating x", service: "web", level: "warn" })).toBe("[warn] web: slot 1: creating x");
    expect(eventLine({ message: "slot 1: creating x", service: "web", level: "info" }, "web")).toBe("slot 1: creating x");
    expect(eventLine(ev("deployed")).endsWith(" deployed")).toBe(true);
    expect(eventServices([...rollout, ev("x", "db"), ev("y", "web")])).toEqual(["web", "db"]);
  });

  it("says what it deployed, in the grid and in Recent deployments", () => {
    expect(stackSource({ action: "deploy" })).toBe("Compose file");
    expect(stackSource({ action: "rollback", rollback_of: 1 })).toBe("Rollback to #1");
    expect(stackSource({ action: "env" })).toBe("Environment changed");
    expect(stackSource({ action: "domains" })).toBe("Domains changed");
    expect(stackRecentLabel({ id: 2, trigger: "api", status: "done", created_at: 0, action: "deploy", services: ["web"] })).toBe("web");
    expect(stackRecentLabel({ id: 3, trigger: "api", status: "done", created_at: 0, action: "deploy", services: [] })).toBe("manual");
    expect(stackRecentLabel({ id: 4, trigger: "api", status: "done", created_at: 0, action: "rollback", rollback_of: 1 })).toBe("rollback to #1");
  });
});
