import { describe, expect, it } from "vitest";
import { defaultEnvironment, defaultProject, NEW } from "./where";

const p = (name: string, created_at: number, envs: string[] = []) => ({ name, created_at, environments: envs.map((n) => ({ name: n })) });

describe("where a template deploys", () => {
  it("defaults to a new project named after the template when there are none", () => {
    expect(defaultProject([], "umami")).toEqual({ choice: NEW, newName: "umami" });
  });
  it("preselects the only project, or the newest of several", () => {
    expect(defaultProject([p("web", 1)], "umami").choice).toBe("web");
    expect(defaultProject([p("old", 1), p("fresh", 9), p("mid", 5)], "umami").choice).toBe("fresh");
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
