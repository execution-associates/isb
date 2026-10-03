import { describe, expect, it } from "vitest";
import { stepStates } from "@/lib/status";
import type { Deployment } from "./api";
import { buildStepLabel, DeploymentFollow, isMine, markMine, queuedEvent } from "./follow";

const dep = (over: Partial<Deployment> = {}): Deployment => ({
  id: 3,
  app: "web",
  trigger: "api",
  by: "ada@example.com",
  status: "queued",
  created_at: 1_000,
  ...over,
});

describe("DeploymentFollow", () => {
  it("starts from the record it is given and follows replies in place", () => {
    const f = new DeploymentFollow(dep());
    expect(f.record?.status).toBe("queued");
    expect(f.apply(0, { log: "image docker:nginx\n", offset: 19, finished: false, deployment: dep({ status: "building", started_at: 2_000 }) }, 5_000)).toBe(true);
    expect(f.record?.status).toBe("building");
    expect(f.firstLineAt).toBe(5_000);
    expect(f.lastLine).toBe("image docker:nginx");
    expect(f.reached.building).toBe(true);
    f.apply(19, { log: "stack web create\n", offset: 36, finished: false, deployment: dep({ status: "deploying", image: "docker:nginx" }) }, 6_000);
    expect(f.firstLineAt).toBe(5_000);
    f.apply(36, { log: "deployment 3 done\n", offset: 54, finished: true, deployment: dep({ status: "done", image: "docker:nginx" }) });
    expect(f.finished).toBe(true);
    expect(f.log.buf.all()).toEqual(["image docker:nginx", "stack web create", "deployment 3 done"]);
    expect(stepStates(f.record!.status, f.reached)).toEqual({ queued: "done", building: "done", deploying: "done", done: "done" });
  });

  it("ignores a stale reply and never moves status back", () => {
    const f = new DeploymentFollow();
    f.apply(0, { log: "a\n", offset: 2, finished: false, deployment: dep({ status: "deploying" }) });
    // A slow request made at offset 0 answers late.
    expect(f.apply(0, { log: "a\n", offset: 2, finished: false, deployment: dep({ status: "building" }) })).toBe(false);
    f.setRecord(dep({ status: "building" }));
    expect(f.record?.status).toBe("deploying");
    expect(f.log.buf.all()).toEqual(["a"]);
  });

  it("works with an older daemon that answers status only", () => {
    const f = new DeploymentFollow(dep());
    f.apply(0, { log: "", offset: 0, finished: false, status: "building" });
    expect(f.record?.status).toBe("building");
    expect(f.firstLineAt).toBeNull();
  });

  it("places a failure in the step it happened", () => {
    const build = new DeploymentFollow(dep({ status: "failed", started_at: 2 }));
    expect(stepStates("failed", build.reached)).toEqual({ queued: "done", building: "failed", deploying: "skipped", done: "skipped" });
    const rollout = new DeploymentFollow(dep({ status: "failed", started_at: 2, image: "x" }));
    expect(stepStates("failed", rollout.reached).deploying).toBe("failed");
    const early = new DeploymentFollow(dep({ status: "failed" }));
    expect(stepStates("failed", early.reached).queued).toBe("failed");
  });

  it("reads the event feed", () => {
    const f = new DeploymentFollow(dep());
    expect(f.onEvent("app web: #3: Step 2/5 : RUN npm ci", "web", 3)).toBe("line");
    expect(f.lastLine).toBe("Step 2/5 : RUN npm ci");
    expect(f.onEvent("app web: deployment 3: deploying", "web", 3)).toBe("state");
    expect(f.onEvent("app web: #30: x", "web", 3)).toBeNull();
    expect(f.onEvent("app api: #3: x", "web", 3)).toBeNull();
  });
});

describe("helpers", () => {
  it("names the build step for what it does", () => {
    expect(buildStepLabel({ rollback_of: 2 }, true)).toBe("Restore");
    expect(buildStepLabel({}, true)).toBe("Build");
    expect(buildStepLabel({}, false)).toBe("Pull");
  });
  it("parses a queued event", () => {
    expect(queuedEvent("app web: deployment 12 queued by webhook:github")).toEqual({ app: "web", id: 12, by: "webhook:github" });
    expect(queuedEvent("app web: deployment 12: building")).toBeNull();
  });
  it("remembers the deployments this tab started", () => {
    markMine("acme", "web", 4);
    expect(isMine("acme", "web", 4)).toBe(true);
    expect(isMine("acme", "web", 5)).toBe(false);
  });
});
