import { describe, expect, it } from "vitest";
import { type DeployAnswer, emptyMeans, filterTemplates, followOf, formProblems, tagCounts, type TemplateSummary, valuesToSend, type Variable, variableProblem } from "./api";

const v = (p: Partial<Variable>): Variable => ({ name: "x", required: false, generated: false, ...p });

describe("template variables", () => {
  it("requires a text variable without a default, never a generated one", () => {
    expect(variableProblem(v({ type: "string", required: true }), "")).toBe("Required.");
    expect(variableProblem(v({ type: "password", required: false, generated: true }), "")).toBeNull();
    expect(variableProblem(v({ type: "domain" }), "")).toBeNull();
  });

  it("checks values by type as the daemon does", () => {
    expect(variableProblem(v({ type: "email" }), "a@b.co")).toBeNull();
    expect(variableProblem(v({ type: "email" }), "a@b")).toBe("An email address.");
    expect(variableProblem(v({ type: "email" }), "a b@c.d")).toBe("An email address.");
    expect(variableProblem(v({ type: "url" }), "ftp://x")).toBe("An http(s) URL.");
    expect(variableProblem(v({ type: "url" }), "https://x.example")).toBeNull();
    expect(variableProblem(v({ type: "int", min: 1, max: 10 }), "11")).toBe("Between 1 and 10.");
    expect(variableProblem(v({ type: "int" }), "1.5")).toBe("A whole number.");
    expect(variableProblem(v({ type: "port" }), "70000")).toBe("Between 1 and 65535.");
    expect(variableProblem(v({ type: "domain" }), "stats.example.com")).toBeNull();
    expect(variableProblem(v({ type: "domain" }), "auto")).toBeNull();
    expect(variableProblem(v({ type: "domain" }), "localhost")).toMatch(/hostname/);
    expect(variableProblem(v({ type: "domain" }), "-bad.example.com")).toMatch(/hostname/);
  });

  it("checks lengths and choices before types", () => {
    expect(variableProblem(v({ min_length: 3 }), "ab")).toBe("At least 3 characters.");
    expect(variableProblem(v({ max_length: 2 }), "abc")).toBe("At most 2 characters.");
    expect(variableProblem(v({ choices: ["a", "b"] }), "c")).toBe("One of a, b.");
    expect(variableProblem(v({ type: "password", generated: true, min_length: 12 }), "short")).toBe("At least 12 characters.");
  });

  it("collects problems for a form and sends only what was typed", () => {
    const vars = [v({ name: "domain", type: "domain" }), v({ name: "admin", type: "email", required: true }), v({ name: "pw", type: "password", generated: true })];
    expect(formProblems(vars, {})).toEqual({ admin: "Required." });
    expect(formProblems(vars, { admin: "me@example.com", domain: "nope" })).toEqual({ domain: expect.stringMatching(/hostname/) });
    expect(valuesToSend({ admin: "me@example.com", pw: "", domain: "" })).toEqual({ admin: "me@example.com" });
  });

  it("says what an empty field becomes", () => {
    expect(emptyMeans(v({ type: "password", generated: true }))).toBe("generated: 32 letters and digits");
    expect(emptyMeans(v({ type: "hex", generated: true, bytes: 16 }))).toBe("generated: 16 random bytes, hex");
    expect(emptyMeans(v({ type: "domain" }))).toBe("generated name");
    expect(emptyMeans(v({ default: "admin" }))).toBe("admin");
  });
});

describe("catalog search", () => {
  const t = (id: string, tags: string[], description = ""): TemplateSummary => ({
    ref: `builtin/${id}`,
    catalog: "builtin",
    id,
    name: id,
    description,
    tags,
    links: {},
    format: "native",
  });
  const all = [t("umami", ["analytics"], "web analytics"), t("plausible", ["analytics"]), t("gitea", ["git", "dev"], "a forge")];

  it("matches every word, and a tag", () => {
    expect(filterTemplates(all, "web anal", null).map((x) => x.id)).toEqual(["umami"]);
    expect(filterTemplates(all, "", "analytics").map((x) => x.id)).toEqual(["umami", "plausible"]);
    expect(filterTemplates(all, "forge", "analytics")).toEqual([]);
  });

  it("counts tags, most used first", () => {
    expect(tagCounts(all)).toEqual([
      ["analytics", 2],
      ["dev", 1],
      ["git", 1],
    ]);
  });
});

describe("following a template deploy", () => {
  const answer = (first?: { app: string; id: number }, deploying?: string[]): DeployAnswer =>
    ({ ref: "builtin/x", plan: { order: ["x-db", "x-cache", "x"] } as DeployAnswer["plan"], first_deployment: first, deploying }) as DeployAnswer;

  it("opens the first app's deployment and chains the rest in order", () => {
    expect(followOf(answer({ app: "x-db", id: 4 }))).toEqual({ app: "x-db", id: 4, then: ["x-cache", "x"] });
    expect(followOf(answer({ app: "x-db", id: 4 }, ["x-db", "x"]))).toEqual({ app: "x-db", id: 4, then: ["x"] });
  });

  it("falls back when no deployment was queued", () => {
    expect(followOf(answer())).toBeNull();
  });
});
