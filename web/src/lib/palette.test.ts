import { describe, expect, it } from "vitest";
import { type Command, filterCommands, groupCommands, move, scoreCommand, scoreToken, sequence } from "./palette";

const c = (title: string, group = "Apps", keywords: string[] = []): Command => ({ id: title, title, group, keywords });

describe("scoreToken", () => {
  it("ranks exact, prefix, word prefix, substring, subsequence", () => {
    const exact = scoreToken("web", "web");
    const prefix = scoreToken("web", "webhooks");
    const word = scoreToken("web", "storefront / web");
    const sub = scoreToken("eb", "web");
    const seq = scoreToken("wb", "web");
    expect(exact).toBeGreaterThan(prefix);
    expect(prefix).toBeGreaterThan(word);
    expect(word).toBeGreaterThan(sub);
    expect(sub).toBeGreaterThan(seq);
    expect(seq).toBeGreaterThan(0);
  });
  it("refuses characters out of order", () => {
    expect(scoreToken("bw", "web")).toBe(0);
    expect(scoreToken("x", "web")).toBe(0);
  });
  it("is case-insensitive on the text", () => {
    expect(scoreToken("post", "PostgreSQL")).toBeGreaterThan(90);
  });
});

describe("scoreCommand", () => {
  it("needs every token to match", () => {
    expect(scoreCommand("deploy web", c("Deploy web", "Actions"))).toBeGreaterThan(0);
    expect(scoreCommand("deploy api", c("Deploy web", "Actions"))).toBe(0);
  });
  it("finds a command by keyword", () => {
    expect(scoreCommand("storefront", c("web", "Apps", ["storefront", "production"]))).toBeGreaterThan(0);
  });
  it("weights the title over keywords", () => {
    expect(scoreCommand("api", c("api", "Apps"))).toBeGreaterThan(scoreCommand("api", c("web", "Apps", ["api"])));
  });
});

describe("filterCommands", () => {
  const all = [c("Overview", "Navigate"), c("Projects", "Navigate"), c("web", "Apps", ["storefront"]), c("Deploy web", "Actions"), c("api", "Apps")];
  it("keeps the given order without a query", () => {
    expect(filterCommands(all, "").map((x) => x.title)).toEqual(["Overview", "Projects", "web", "Deploy web", "api"]);
    expect(filterCommands(all, "   ", 2)).toHaveLength(2);
  });
  it("puts the best match first", () => {
    expect(filterCommands(all, "web").map((x) => x.title)).toEqual(["web", "Deploy web"]);
    expect(filterCommands(all, "pro")[0].title).toBe("Projects");
  });
  it("drops what does not match", () => {
    expect(filterCommands(all, "zzz")).toEqual([]);
  });
});

describe("groupCommands", () => {
  it("groups in order of first appearance", () => {
    const g = groupCommands([c("a", "X"), c("b", "Y"), c("c", "X")]);
    expect(g.map((x) => [x.group, x.items.map((i) => i.title)])).toEqual([
      ["X", ["a", "c"]],
      ["Y", ["b"]],
    ]);
  });
});

describe("move", () => {
  it("wraps around both ends", () => {
    expect(move(0, -1, 3)).toBe(2);
    expect(move(2, 1, 3)).toBe(0);
    expect(move(1, 1, 3)).toBe(2);
    expect(move(5, 1, 0)).toBe(0);
  });
});

describe("sequence", () => {
  const targets = { p: "/projects", h: "/history" };
  it("fires on g then a known key", () => {
    const a = sequence(null, "g", targets);
    expect(a).toEqual({ fire: null, pending: "g" });
    expect(sequence(a.pending, "p", targets)).toEqual({ fire: "/projects", pending: null });
  });
  it("forgets after an unknown key", () => {
    expect(sequence("g", "z", targets)).toEqual({ fire: null, pending: null });
    expect(sequence(null, "p", targets)).toEqual({ fire: null, pending: null });
  });
});
