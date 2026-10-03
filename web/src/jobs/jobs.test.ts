import { describe, expect, it } from "vitest";
import { formOf, formProblems, settingsOf } from "@/previews/api";
import { durationSeconds, joinWords, splitWords } from "./api";

describe("job commands", () => {
  it("splits a line as the daemon does (src/flex.rs)", () => {
    expect(splitWords("./manage.py prune --days 30")).toEqual(["./manage.py", "prune", "--days", "30"]);
    expect(splitWords(`sh -c 'echo "hi there" && date'`)).toEqual(["sh", "-c", 'echo "hi there" && date']);
    expect(splitWords(`echo "a \\"b\\" \\n" c\\ d`)).toEqual(["echo", 'a "b" \\n', "c d"]);
    expect(splitWords(`''`)).toEqual([""]);
    expect(() => splitWords("echo 'open")).toThrow();
    expect(() => splitWords('echo "open')).toThrow();
  });

  it("joins argv back to a line that splits the same", () => {
    for (const argv of [["ls", "-la"], ["sh", "-c", "echo it's && date"], ["a b", "", "$HOME"]]) {
      expect(splitWords(joinWords(argv))).toEqual(argv);
    }
  });

  it("reads durations as the daemon does", () => {
    expect(durationSeconds("10m")).toBe(600);
    expect(durationSeconds("90")).toBe(90);
    expect(durationSeconds("1.5h")).toBe(5400);
    expect(durationSeconds("250ms")).toBe(0.25);
    expect(durationSeconds("1h30m")).toBeNull();
    expect(durationSeconds("soon")).toBeNull();
  });
});

describe("preview settings form", () => {
  it("round-trips saved settings", () => {
    const saved = { enabled: true, branches: ["main", "dev"], max: 5, replicas: 1, domain: "*.pr.example.com", inherit_env: true, env: "MODE=preview\n", forks: true, fork_secrets: ["A"], status: { token_secret: "tok", kind: "gitea" as const } };
    expect(settingsOf(formOf(saved))).toEqual(saved);
    expect(settingsOf(formOf(undefined))).toEqual({ enabled: false, max: 3, replicas: 1, domain: "auto", inherit_env: false, forks: false });
  });

  it("validates as src/app/preview.rs does", () => {
    const f = formOf({ enabled: true });
    expect(formProblems(f, 3000)).toEqual({});
    expect(formProblems(f, null).port).toBeTruthy();
    expect(formProblems({ ...f, max: "51", replicas: "0", domain: "pr.example.com", ttl: "5m" }, 80)).toEqual({
      max: "1 to 50.",
      replicas: "1 to 10.",
      domain: expect.stringContaining("auto"),
      ttl: expect.stringContaining("10m"),
    });
  });

  it("drops fork secrets when forks are off", () => {
    expect(settingsOf({ ...formOf({}), forks: false, fork_secrets: ["X"] }).fork_secrets).toBeUndefined();
  });
});
