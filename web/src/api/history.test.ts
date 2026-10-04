import { describe, expect, it } from "vitest";
import { type HistoryItem, historyArgs, historyStreamUrl, itemKey, itemMatches } from "@/api/history";

const item = (over: Partial<HistoryItem> = {}): HistoryItem => ({
  source: "incus",
  id: 4,
  time: 1_000_000,
  org: "acme",
  kind: "instance-deleted",
  object_type: "instance",
  object: "web-1-ab12",
  actor: "stephan",
  level: null,
  message: null,
  details: {},
  ...over,
});

describe("history filters", () => {
  it("match live items as the server would", () => {
    expect(itemMatches(item(), {})).toBe(true);
    expect(itemMatches(item(), { source: "audit,controller" })).toBe(false);
    expect(itemMatches(item(), { source: "incus" })).toBe(true);
    expect(itemMatches(item(), { kind: "instance-*" })).toBe(true);
    expect(itemMatches(item(), { kind: "deploy.*" })).toBe(false);
    expect(itemMatches(item(), { object: "web-1" })).toBe(true);
    expect(itemMatches(item(), { object: "web-1", exact: true })).toBe(false);
    expect(itemMatches(item({ org: null }), { platform: true })).toBe(true);
    expect(itemMatches(item(), { platform: true })).toBe(false);
    expect(itemMatches(item(), { actor: "steph*" })).toBe(true);
    expect(itemMatches(item(), { since: 2_000_000 })).toBe(false);
  });
  it("send only what is set, and key items per source", () => {
    expect(historyArgs({ org: "acme", object: " ", platform: false }, { before: "1.2.3", limit: 50 })).toEqual({
      org: "acme",
      before: "1.2.3",
      limit: 50,
    });
    expect(historyStreamUrl("acme")).toBe("/api/v1/history/stream?org=acme");
    expect(itemKey(item())).toBe("incus:4");
  });
});
