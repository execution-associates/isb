import { describe, expect, it } from "vitest";
import { appVerdict, diffStats, fold, isEdited, lineDiff, problemText, stackVerdict } from "./yaml-edit";

const kinds = (a: string, b: string) => lineDiff(a, b).map((r) => (r.kind === "same" ? " " : r.kind === "add" ? "+" : "-") + r.text);

describe("lineDiff", () => {
  it("is all unchanged for equal texts", () => {
    expect(lineDiff("a\nb\n", "a\nb\n").every((r) => r.kind === "same")).toBe(true);
    expect(lineDiff("", "")).toEqual([]);
  });

  it("shows an edit as a removal and an addition in place", () => {
    expect(kinds("name: web\nreplicas: 1\nport: 80\n", "name: web\nreplicas: 3\nport: 80\n")).toEqual([" name: web", "-replicas: 1", "+replicas: 3", " port: 80"]);
  });

  it("numbers old and new lines on their own counters", () => {
    const rows = lineDiff("a\nb\nc\n", "a\nx\ny\nc\n");
    expect(rows.map((r) => [r.kind, r.oldNo, r.newNo])).toEqual([
      ["same", 1, 1],
      ["del", 2, undefined],
      ["add", undefined, 2],
      ["add", undefined, 3],
      ["same", 3, 4],
    ]);
  });

  it("handles a new document and a cleared one", () => {
    expect(kinds("", "a\nb\n")).toEqual(["+a", "+b"]);
    expect(kinds("a\nb\n", "")).toEqual(["-a", "-b"]);
  });

  it("keeps the longest common lines when an insertion moves things down", () => {
    expect(kinds("a\nb\nc\n", "a\nnew\nb\nc\n")).toEqual([" a", "+new", " b", " c"]);
  });

  it("counts what changed", () => {
    expect(diffStats(lineDiff("a\nb\nc\n", "a\nB\nc\nd\n"))).toEqual({ added: 2, removed: 1 });
  });

  it("does not freeze on a huge edit", () => {
    const old = Array.from({ length: 3000 }, (_, i) => `o${i}`).join("\n");
    const neu = Array.from({ length: 3000 }, (_, i) => `n${i}`).join("\n");
    const rows = lineDiff(old, neu);
    expect(diffStats(rows)).toEqual({ added: 3000, removed: 3000 });
  });
});

describe("fold", () => {
  const long = Array.from({ length: 30 }, (_, i) => `l${i}`).join("\n") + "\n";

  it("keeps three lines around a change and folds the rest", () => {
    const shown = fold(lineDiff(long, long.replace("l15\n", "L15\n")));
    expect(shown[0]).toEqual({ kind: "gap", hidden: 12 });
    expect(shown.filter((s) => s.kind !== "gap")).toHaveLength(8);
    expect(shown.at(-1)).toEqual({ kind: "gap", hidden: 11 });
  });

  it("joins hunks that are close and splits those that are far", () => {
    const close = fold(lineDiff(long, long.replace("l10\n", "L10\n").replace("l14\n", "L14\n")));
    expect(close.filter((s) => s.kind === "gap")).toHaveLength(2);
    const far = fold(lineDiff(long, long.replace("l2\n", "L2\n").replace("l27\n", "L27\n")));
    expect(far.filter((s) => s.kind === "gap")).toHaveLength(1);
  });

  it("shows nothing for an unchanged text", () => {
    expect(fold(lineDiff(long, long))).toEqual([]);
  });
});

describe("isEdited", () => {
  it("ignores a trailing newline", () => {
    expect(isEdited("a: 1", "a: 1\n")).toBe(false);
    expect(isEdited("a: 1\n\n", "a: 1\n")).toBe(false);
    expect(isEdited("a: 2", "a: 1\n")).toBe(true);
  });
});

describe("appVerdict", () => {
  it("allows a valid edit of this app", () => {
    const v = appVerdict({ valid: true, action: "updated", name: "web", changes: ["env", "replicas"] }, "web");
    expect(v).toEqual({ ok: true, problems: [], changes: ["env", "replicas"] });
  });

  it("carries the server's problems, with their lines", () => {
    const v = appVerdict({ valid: false, errors: [{ line: 5, message: "replicas: at most 100" }] }, "web");
    expect(v.ok).toBe(false);
    expect(v.problems).toEqual([{ line: 5, message: "replicas: at most 100" }]);
    expect(problemText(v.problems[0])).toBe("Line 5: replicas: at most 100");
  });

  it("carries what the document removes, for the review to confirm", () => {
    const v = appVerdict({ valid: true, action: "updated", name: "web", changes: ["env"], removals: ["env: TOKEN", "domains: a.example.com"] }, "web");
    expect(v.ok).toBe(true);
    expect(v.removals).toEqual(["env: TOKEN", "domains: a.example.com"]);
    expect(appVerdict({ valid: true, action: "updated", name: "web", removals: [] }, "web")).not.toHaveProperty("removals");
  });

  it("refuses to save what would create another app", () => {
    const v = appVerdict({ valid: true, action: "created", name: "api", changes: ["name"] }, "web");
    expect(v.ok).toBe(false);
    expect(v.blocked).toMatch(/different app \(api\)/);
  });
});

describe("stackVerdict", () => {
  const base = { name: "shop", creating: false };

  it("lists the services that change and skips those that do not", () => {
    const v = stackVerdict({ valid: true, exists: true, changes: [{ service: "api", change: "update" }, { service: "db", change: "unchanged" }] }, base);
    expect(v).toEqual({ ok: true, problems: [], changes: ["api update"] });
  });

  it("refuses a stack that belongs to apps", () => {
    const v = stackVerdict({ valid: true, managed_by: "apps", changes: [] }, base);
    expect(v.ok).toBe(false);
    expect(v.blocked).toMatch(/project's apps/);
  });

  it("refuses to create over an existing stack", () => {
    const v = stackVerdict({ valid: true, exists: true, changes: [] }, { ...base, creating: true });
    expect(v.ok).toBe(false);
    expect(v.blocked).toMatch(/already exists/);
    expect(stackVerdict({ valid: true, exists: false, changes: [] }, { ...base, creating: true }).ok).toBe(true);
  });

  it("is not ok for an invalid file, whatever else holds", () => {
    const v = stackVerdict({ valid: false, errors: [{ line: 3, message: "unknown field `x`" }] }, base);
    expect(v.ok).toBe(false);
    expect(v.problems).toHaveLength(1);
  });
});
