import { describe, expect, it } from "vitest";
import { autoHostLabel, ingressOff, NO_INGRESS_WARNING, domainFromSpec, domainToSpec, emptyDomain, hostProblem, matchStatuses, normalizePath, validateDomain, type DomainForm } from "./domains";
import { analyzeEnv, missingSecrets } from "./envtext";
import { concernsDeployment, deploymentLine, LogBuffer, LogFollower, parseAnsi, stripAnsi } from "./logstream";
import { applyPatch, duration, mergePatch, nameProblem, parseKv, portProblem, shortDigest, terminalUrl, volumeProblem } from "./util";
import { gitUrlProblem } from "./new-app-dialog";
import { parseMemory } from "./app-monitoring";
import { appState, type Project } from "./api";
import { activeServiceTab, SERVICE_TABS, serviceTabs } from "./service-tabs";
import { envHealth, projectHealth } from "./health";
import { lastDeploy, projectCounts } from "./projects-page";

describe("env text analysis (src/app/env.rs rules)", () => {
  const text = [
    "# database",
    "DATABASE_HOST=db.shop-production",
    "export PORT=8080            # export is dropped",
    'GREETING="hello world"      # double quotes',
    "RAW='kept # as is'",
    "API_TOKEN=${{secret.api_token}}",
    "",
  ].join("\n");

  it("reads variables, comments and secret references", () => {
    const a = analyzeEnv(text);
    expect(a.problems).toEqual([]);
    expect([...a.vars.keys()]).toEqual(["DATABASE_HOST", "PORT", "GREETING", "RAW", "API_TOKEN"]);
    expect(a.vars.get("PORT")).toBe("8080");
    expect(a.vars.get("GREETING")).toBe("hello world");
    expect(a.vars.get("RAW")).toBe("kept # as is");
    expect(a.vars.get("API_TOKEN")).toEqual({ secret: "api_token" });
    expect(a.secrets).toEqual(["api_token"]);
  });

  it("keeps one token line per text line, whose text adds up to the line", () => {
    const multi = 'A=1\nB="two\nlines"\n  # indented comment\nC=${{secret.x}} # note';
    const a = analyzeEnv(multi);
    const phys = multi.split("\n");
    expect(a.lines).toHaveLength(phys.length);
    a.lines.forEach((toks, i) => expect(toks.map((t) => t.text).join("")).toBe(phys[i]));
    expect(a.vars.get("B")).toBe("two\nlines");
  });

  it("highlights secret references, and only unquoted whole values", () => {
    const a = analyzeEnv('S=${{secret.db-pass}}\nQ="${{secret.nope}}"\nM=pre${{secret.x}}');
    expect(a.lines[0].find((t) => t.kind === "secret")?.text).toBe("${{secret.db-pass}}");
    expect(a.lines[1].some((t) => t.kind === "secret")).toBe(false);
    expect(a.secrets).toEqual(["db-pass"]);
    const warnings = a.problems.filter((p) => p.severity === "warning").map((p) => p.line);
    expect(warnings).toEqual([2, 3]);
  });

  it.each([
    ["NOEQUALS", 1, "expected KEY=value"],
    ["1BAD=x", 1, "letters, digits and _"],
    ["A-B=x", 1, "letters, digits and _"],
    ['A="open', 1, "unterminated quote"],
    ["A='open", 1, "unterminated quote"],
    ['A="x" trailing', 1, "text after the closing quote"],
    ["A=${{secret..hidden}}", 1, "secret name"],
    ["ok=1\n=nokey", 2, "a variable needs a name"],
  ])("reports %j at line %i", (t, line, msg) => {
    const errs = analyzeEnv(t).problems.filter((p) => p.severity === "error");
    expect(errs).toHaveLength(1);
    expect(errs[0].line).toBe(line);
    expect(errs[0].message).toContain(msg);
  });

  it("warns about a key set twice, and the last one wins", () => {
    const a = analyzeEnv("A=1\nA=2");
    expect(a.vars.get("A")).toBe("2");
    expect(a.problems).toEqual([{ line: 2, message: "A is also set on line 1; this one wins", severity: "warning" }]);
  });

  it("handles escapes and CRLF", () => {
    const a = analyzeEnv('A="a\\"b\\nc"\r\nB=x\r\n');
    expect(a.vars.get("A")).toBe('a"b\nc');
    expect(a.vars.get("B")).toBe("x");
    expect(a.problems).toEqual([]);
  });

  it("finds missing secrets", () => {
    const a = analyzeEnv("A=${{secret.one}}\nB=${{secret.two}}");
    expect(missingSecrets(a, ["one", "other"])).toEqual(["two"]);
  });
});

describe("deployment log stream", () => {
  it("joins chunks split mid-line and never repeats a line", () => {
    const b = new LogBuffer();
    b.push("fetching\nbuil");
    b.push("ding\r\n");
    b.push("");
    b.push("done");
    expect(b.lines).toEqual(["fetching", "building"]);
    expect(b.all()).toEqual(["fetching", "building", "done"]);
    expect(b.text()).toBe("fetching\nbuilding\ndone");
  });

  it("drops the oldest lines past its cap and counts them", () => {
    const b = new LogBuffer(3);
    b.push("1\n2\n3\n4\n5\n");
    expect(b.lines).toEqual(["3", "4", "5"]);
    expect(b.dropped).toBe(2);
  });

  it("follows offsets and ignores a stale reply", () => {
    const f = new LogFollower();
    expect(f.apply(0, { log: "a\nb", offset: 3, finished: false })).toBe(true);
    // A slow request made at offset 0 answering late must not append again.
    expect(f.apply(0, { log: "a\nb", offset: 3, finished: false })).toBe(false);
    expect(f.apply(3, { log: "c\n", offset: 6, finished: true })).toBe(true);
    expect(f.buf.all()).toEqual(["a", "bc"]);
    expect(f.finished).toBe(true);
  });

  it("completes the last line when the deployment finishes", () => {
    const f = new LogFollower();
    f.apply(0, { log: "deployment 1 done", offset: 17, finished: true });
    expect(f.buf.lines).toEqual(["deployment 1 done"]);
    expect(f.buf.partial).toBe("");
  });

  it("recognises a deployment's events", () => {
    expect(deploymentLine("app web: #3: pulling image", "web", 3)).toBe("pulling image");
    expect(deploymentLine("app web: #3: x", "web", 4)).toBeNull();
    expect(deploymentLine("app webx: #3: x", "web", 3)).toBeNull();
    expect(concernsDeployment("app web: deployment 3 done", "web", 3)).toBe(true);
    expect(concernsDeployment("app web: deployment 3: building", "web", 3)).toBe(true);
    expect(concernsDeployment("app web: deployment 3 queued by a@b", "web", 3)).toBe(true);
    expect(concernsDeployment("app web: deployment 31 done", "web", 3)).toBe(false);
  });

  it("parses ANSI colours and drops other escapes", () => {
    expect(parseAnsi("\x1b[31merror\x1b[0m: \x1b[1;32mok\x1b[22m!")).toEqual([
      { text: "error", fg: "red" },
      { text: ": " },
      { text: "ok", fg: "green", bold: true },
      { text: "!", fg: "green" },
    ]);
    expect(parseAnsi("\x1b[2K\x1b[38;5;196mx\x1b[39m")).toEqual([{ text: "x" }]);
    expect(stripAnsi("\x1b[90mdim\x1b[0m")).toBe("dim");
    expect(parseAnsi("plain")).toEqual([{ text: "plain" }]);
  });
});

describe("domain form validation (src/ingress/domain.rs rules)", () => {
  const f = (p: Partial<DomainForm>): DomainForm => ({ ...emptyDomain(), ...p });

  it.each(["app.example.com", "auto", "*.example.com", "a-b.c.example.co"])("accepts host %s", (h) => expect(hostProblem(h)).toBeNull());
  it.each([
    ["", "Enter a hostname"],
    ["localhost", "needs a domain"],
    ["10.0.0.1", "IP address"],
    ["*.com", "wildcard needs a domain"],
    ["-a.example.com", "not a DNS label"],
    ["a_b.example.com", "not a DNS label"],
    ["web.isb", "internal"],
    ["x.localhost", "internal"],
  ])("refuses host %j", (h, msg) => expect(hostProblem(h)).toContain(msg));

  it("normalizes paths like the ingress", () => {
    expect(normalizePath("")).toBe("/");
    expect(normalizePath("/api/")).toBe("/api");
    expect(normalizePath("api")).toEqual({ error: "A path starts with /." });
    expect(normalizePath("/a//b")).toHaveProperty("error");
    expect(normalizePath("/a/../b")).toHaveProperty("error");
    expect(normalizePath("/a?b")).toHaveProperty("error");
  });

  it("needs a port unless the app has one or it redirects", () => {
    expect(validateDomain(f({ host: "a.example.com" }), null, [])).toEqual({ port: "Give a port: the app has no port set." });
    expect(validateDomain(f({ host: "a.example.com" }), 80, [])).toEqual({});
    expect(validateDomain(f({ host: "a.example.com", redirect: "https://b.example.com" }), null, [])).toEqual({});
    expect(validateDomain(f({ host: "a.example.com", redirect: "ftp://x" }), null, []).redirect).toBeTruthy();
    expect(validateDomain(f({ host: "a.example.com", port: "70000" }), null, []).port).toBe("A port is 1-65535.");
  });

  it("refuses strip_prefix on /, www_redirect on auto/www/wildcards, and duplicates", () => {
    expect(validateDomain(f({ host: "a.example.com", strip_prefix: true }), 80, []).strip_prefix).toBeTruthy();
    expect(validateDomain(f({ host: "a.example.com", path: "/api", strip_prefix: true }), 80, [])).toEqual({});
    expect(validateDomain(f({ host: "auto", www_redirect: true }), 80, []).www_redirect).toBeTruthy();
    expect(validateDomain(f({ host: "www.a.com", www_redirect: true }), 80, []).www_redirect).toBeTruthy();
    expect(validateDomain(f({ host: "a.example.com", path: "/api/" }), 80, [f({ host: "A.example.com", path: "/api" })]).host).toContain("already listed");
  });

  it("round-trips the stored spec without defaults", () => {
    const spec = { host: "shop.example.com", path: "/api", port: 3000, strip_prefix: true };
    expect(domainToSpec(domainFromSpec(spec))).toEqual(spec);
    expect(domainToSpec(f({ host: "Auto" }))).toEqual({ host: "auto" });
    expect(domainToSpec(f({ host: "a.b", https: false, redirect: "https://c.d", port: "80" }))).toEqual({ host: "a.b", https: false, redirect: "https://c.d" });
    expect(domainFromSpec({ host: "x.y", https: false })).toMatchObject({ https: false, path: "/" });
  });

  it("pairs configured domains with their live status", () => {
    const forms = [f({ host: "auto" }), f({ host: "a.example.com", path: "/api" })];
    const st = [
      { host: "a.example.com", path: "/api", https: true, provider: "caddy", state: "serving", cert: "issued" },
      { host: "web-shop-acme.127-0-0-1.sslip.io", path: "/", https: true, provider: "caddy", state: "serving", cert: "pending" },
    ];
    const m = matchStatuses(forms, st);
    expect(m[0]?.cert).toBe("pending");
    expect(m[1]?.cert).toBe("issued");
  });
});

describe("merge patches for app_update", () => {
  it("switches an image source to git", () => {
    const from = { image: "docker:nginx" };
    const to = { git: { url: "https://x/y.git", ref: "main" } };
    const p = mergePatch(from, to);
    expect(p).toEqual({ image: null, git: { url: "https://x/y.git", ref: "main" } });
    expect(applyPatch(from, p)).toEqual(to);
  });

  it("swaps git auth kinds and keeps what did not change out of the patch", () => {
    const from = { git: { url: "git@h:o/r", ref: "main", auth: { token_secret: "t", username: "u" } } };
    const to = { git: { url: "git@h:o/r", ref: "main", auth: { ssh_key_secret: "k" } } };
    const p = mergePatch(from, to);
    expect(p).toEqual({ git: { auth: { token_secret: null, username: null, ssh_key_secret: "k" } } });
    expect(applyPatch(from, p)).toEqual(to);
  });

  it("replaces arrays whole", () => {
    expect(mergePatch({ a: [1, 2] }, { a: [2] })).toEqual({ a: [2] });
  });
});

describe("small helpers", () => {
  it("formats durations and digests", () => {
    expect(duration(450)).toBe("450ms");
    expect(duration(4200)).toBe("4.2s");
    expect(duration(64_000)).toBe("1m 04s");
    expect(duration(3_720_000)).toBe("1h 02m");
    expect(shortDigest("sha256:c4717a8d0123456789")).toBe("c4717a8d0123");
  });

  it("checks names, volumes, ports and git URLs like the daemon", () => {
    expect(nameProblem("app", "web-1")).toBeNull();
    expect(nameProblem("app", "Web")).toBeTruthy();
    expect(nameProblem("app", "web-")).toBeTruthy();
    expect(nameProblem("project", "a".repeat(25))).toBeTruthy();
    expect(volumeProblem("data:/var/lib/x:ro")).toBeNull();
    expect(volumeProblem("/host:/x")).toBeTruthy();
    expect(volumeProblem("data:rel")).toBeTruthy();
    expect(portProblem("127.0.0.1:8080:80")).toBeNull();
    expect(portProblem("8080:80/udp")).toBeNull();
    expect(portProblem("80")).toBeTruthy();
    expect(gitUrlProblem("https://github.com/a/b.git")).toBeNull();
    expect(gitUrlProblem("git@github.com:a/b.git")).toBeNull();
    expect(gitUrlProblem("https://user:pw@github.com/a/b")).toContain("credentials");
    expect(gitUrlProblem("file:///etc")).toBeTruthy();
    expect(gitUrlProblem("--upload-pack=x")).toBeTruthy();
    expect(parseKv("A=1\n# c\n\nB = two=2\nbad").map).toEqual({ A: "1", B: "two=2" });
    expect(parseKv("bad").errors).toHaveLength(1);
    expect(parseMemory("512m")).toBe(512 * 1024 ** 2);
    expect(parseMemory("2GiB")).toBe(2 * 1024 ** 3);
  });

  it("builds the terminal websocket URL on this origin", () => {
    expect(terminalUrl({ protocol: "https:", host: "isb.example.com" }, "acme", "web", "auto", 120, 40)).toBe(
      "wss://isb.example.com/orgs/acme/api/v1/terminal?app=web&cols=120&rows=40",
    );
    expect(terminalUrl({ protocol: "http:", host: "localhost:8092" }, "a b", "web", "2", 80, 24)).toBe(
      "ws://localhost:8092/orgs/a%20b/api/v1/terminal?app=web&cols=80&rows=24&slot=2",
    );
  });

  it("derives an app's state from its service and latest deployment", () => {
    const svc = (p: Record<string, unknown>) => ({ service: "web", image: "", rev: "", replicas: 2, running: 2, healthy: 2, state: "converged", instances: [], ports: [], checked_at: 0, ...p }) as never;
    const dep = (status: string) => ({ id: 1, status }) as never;
    expect(appState(undefined, undefined)).toBe("not-deployed");
    expect(appState(undefined, dep("failed"))).toBe("failed");
    expect(appState(svc({}), dep("building"))).toBe("deploying");
    expect(appState(svc({}), dep("done"))).toBe("running");
    expect(appState(svc({ healthy: 1 }), dep("done"))).toBe("degraded");
    expect(appState(svc({ healthy: 0 }), dep("done"))).toBe("failing");
    expect(appState(svc({ replicas: 0, healthy: 0 }), dep("done"))).toBe("stopped");
  });
});

describe("ingress warning", () => {
  it("is off only when the ingress says enabled: false", () => {
    expect(ingressOff({ enabled: false })).toBe(true);
    expect(ingressOff({ enabled: true })).toBe(false);
    expect(ingressOff(undefined)).toBe(false);
  });
  it("labels an auto host: the URL when served, not served when off", () => {
    expect(autoHostLabel("auto", "https://web-shop.203-0-113-7.sslip.io/", false)).toBe("web-shop.203-0-113-7.sslip.io");
    expect(autoHostLabel("auto", undefined, true)).toBe("auto (not served: no ingress)");
    expect(autoHostLabel("shop.example.com", undefined, true)).toBe("shop.example.com (not served: no ingress)");
    expect(autoHostLabel("auto", undefined, false)).toBe("Generated name");
  });
  it("says what to do about it", () => {
    expect(NO_INGRESS_WARNING).toContain("--ingress-https");
  });
});

describe("project cards", () => {
  const shop: Project = {
    name: "shop",
    created_at: 0,
    environments: [
      { name: "production", stack: "shop-production", apps: ["web", "db"], compose: [{ name: "monitoring", services: ["prometheus", "grafana"] }] },
      { name: "staging", stack: "shop-staging", apps: ["web-staging"], compose: [{ name: "search", services: ["meili"] }] },
    ],
  };
  const svc = (healthy: number) => ({ service: "x", image: "", rev: "", replicas: 1, running: 1, healthy, state: "converged", instances: [], ports: [], checked_at: 0 });
  const stack = (name: string, healthy: number, deployed_at = 0) => ({ name, org: "acme", deployed_at, services: [svc(healthy)] });

  it("counts apps, databases and compose stacks", () => {
    expect(projectCounts(shop, new Set(["db"]))).toEqual({ apps: 2, dbs: 1, compose: 2 });
  });

  it("takes health from compose stacks too", () => {
    const stacks = [stack("shop-production", 1), stack("monitoring", 0), stack("shop-staging", 1)];
    expect(envHealth(shop.environments[0], stacks, "acme")).toBe("failing");
    expect(envHealth(shop.environments[1], stacks, "acme")).toBe("healthy");
    expect(projectHealth(shop, stacks, "acme")).toBe("failing");
    // Another org's stack of the same name is not this one.
    expect(envHealth(shop.environments[0], [{ ...stack("monitoring", 0), org: "other" }], "acme")).toBe("idle");
  });

  it("dates the last deploy from apps' deployments and compose stacks' deploys", () => {
    const latest = new Map([["web", [{ created_at: 5_000 } as never]]]);
    expect(lastDeploy(shop, latest, [], "acme")).toBe(5_000);
    expect(lastDeploy(shop, latest, [stack("search", 1, 9)], "acme")).toBe(9_000);
    expect(lastDeploy(shop, new Map(), [], "acme")).toBeUndefined();
  });
});

describe("service tabs", () => {
  const ids = (k: Parameters<typeof serviceTabs>[0]) => serviceTabs(k).map((t) => t.id);

  it("come in one order for every kind, settings first and Advanced last", () => {
    expect(SERVICE_TABS.at(-1)?.id).toBe("advanced");
    expect(ids({ writer: true })).toEqual(["general", "domains", "environment", "deployments", "logs", "monitoring", "terminal", "jobs", "yaml", "advanced"]);
  });

  it("put a git app's Previews right after Deployments", () => {
    expect(ids({ writer: true, git: true })).toEqual(["general", "domains", "environment", "deployments", "previews", "logs", "monitoring", "terminal", "jobs", "yaml", "advanced"]);
  });

  it("give a database Database and Backups first, in place of General and Domains", () => {
    expect(ids({ writer: true, database: true })).toEqual(["database", "backups", "environment", "deployments", "logs", "monitoring", "terminal", "jobs", "yaml", "advanced"]);
  });

  it("leave the terminal out for viewers", () => {
    expect(ids({ writer: false })).not.toContain("terminal");
  });

  it("open on General without a tab, or Database for a database", () => {
    expect(activeServiceTab(undefined, serviceTabs({ writer: true }))).toBe("general");
    expect(activeServiceTab("nope", serviceTabs({ writer: true, git: true }))).toBe("general");
    expect(activeServiceTab(undefined, serviceTabs({ writer: true, database: true }))).toBe("database");
    expect(activeServiceTab("yaml", serviceTabs({ writer: true, database: true }))).toBe("yaml");
    expect(activeServiceTab("previews", serviceTabs({ writer: true }))).toBe("general");
  });
});
