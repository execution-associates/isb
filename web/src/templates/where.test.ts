import { describe, expect, it } from "vitest";
import { defaultEnvironment, defaultProject, NEW, projectSlug } from "./where";

const p = (name: string, created_at: number, envs: string[] = []) => ({ name, created_at, environments: envs.map((n) => ({ name: n })) });

describe("where a template deploys", () => {
  it("defaults to a new project named after the template when there are none", () => {
    expect(defaultProject([], "umami")).toEqual({ choice: NEW, newName: "umami" });
  });
  it("names the new project after the template's display name", () => {
    expect(defaultProject([], "Uptime Kuma")).toEqual({ choice: NEW, newName: "uptime-kuma" });
    expect(projectSlug("Postgres with Adminer")).toBe("postgres-with-adminer");
    expect(projectSlug("Plausible Analytics (Community Edition)")).toBe("plausible-analytics-comm");
    expect(projectSlug("n8n")).toBe("n8n");
    expect(projectSlug("123")).toBe("app");
  });
  it("defaults to a new project even when projects exist, numbering a taken name", () => {
    expect(defaultProject([p("web", 1)], "umami")).toEqual({ choice: NEW, newName: "umami" });
    expect(defaultProject([p("umami", 1), p("umami-2", 2)], "umami")).toEqual({ choice: NEW, newName: "umami-3" });
  });
  it("honors a project from the link, existing or not", () => {
    expect(defaultProject([p("web", 1), p("api", 2)], "umami", "web").choice).toBe("web");
    expect(defaultProject([p("web", 1)], "umami", "shop")).toEqual({ choice: NEW, newName: "shop" });
  });
  it("prefers production, then the first environment, else a new production", () => {
    expect(defaultEnvironment([{ name: "staging" }, { name: "production" }]).choice).toBe("production");
    expect(defaultEnvironment([{ name: "staging" }]).choice).toBe("staging");
    expect(defaultEnvironment([])).toEqual({ choice: NEW, newName: "production" });
    expect(defaultEnvironment([{ name: "production" }], "qa")).toEqual({ choice: NEW, newName: "qa" });
  });
});
