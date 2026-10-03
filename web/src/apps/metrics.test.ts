import { describe, expect, it } from "vitest";
import { bridge, latest, maxOf, niceMax, rate, replicaLabel, stepFor, toGrid } from "./metrics";

describe("bridging", () => {
  it("fills a lone gap and keeps longer ones", () => {
    expect(bridge([1, null, 3, null, null, 6, null])).toEqual([1, 2, 3, null, null, 6, null]);
  });
});

describe("metrics shaping", () => {
  const s = (name: string, points: [number, number][]) => ({ name, stack: "shop-production", service: "web", points });

  it("puts series on one grid, gaps as null, and sums them", () => {
    const g = toGrid(
      [
        s("shop-production-web-1-aa", [
          [1000, 10],
          [1010, 20],
          [1030, 40],
        ]),
        s("shop-production-web-2-bb", [
          [1010, 1],
          [1020, 2],
        ]),
      ],
      1000,
      1040,
      10,
    );
    expect(g.times).toEqual([1000, 1010, 1020, 1030]);
    expect(g.lines[0].values).toEqual([10, 20, null, 40]);
    expect(g.lines[1].values).toEqual([null, 1, 2, null]);
    expect(g.total).toEqual([10, 21, 2, 40]);
    expect(g.counts).toEqual([1, 2, 1, 1]);
  });

  it("aligns to the step and averages points sharing a bucket", () => {
    const g = toGrid(
      [
        s("a", [
          [1000, 1],
          [1010, 3],
          [1060, 5],
        ]),
      ],
      1005,
      1120,
      60,
    );
    expect(g.times).toEqual([960, 1020, 1080]);
    expect(g.lines[0].values).toEqual([2, 5, null]);
  });

  it("keeps an all-gap grid empty rather than zero", () => {
    const g = toGrid([], 0, 30, 10);
    expect(g.total).toEqual([null, null, null]);
    expect(latest(g.total)).toBeNull();
  });

  it("scales axes and labels", () => {
    expect(niceMax(0)).toBe(1);
    expect(niceMax(3.2)).toBe(5);
    expect(niceMax(120)).toBe(200);
    expect(niceMax(1000)).toBe(1000);
    expect(maxOf([[1, null, 7], [3]], 2)).toBe(7);
    expect(latest([1, 2, null])).toBe(2);
    expect(rate(1536)).toBe("1.5 KiB/s");
    expect(rate(null)).toBe("–");
    expect(replicaLabel("shop-production-web-2-5b0c", "web")).toBe("replica 2");
    expect(replicaLabel("odd-name", "web")).toBe("odd-name");
  });

  it("asks for about 240 points per range", () => {
    expect(stepFor("1h")).toBe(20);
    expect(stepFor("24h")).toBe(360);
    expect(stepFor("7d")).toBe(2520);
    expect(stepFor("30d")).toBe(10800);
  });
});
