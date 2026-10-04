// The same cases as src/cron.rs's tests, so the preview agrees with the
// daemon's scheduler.
import { describe, expect, it } from "vitest";
import { cronPreview, describeCron, formatRun, nextAfter, nextRuns, parseCron, parseOffset } from "./cron";

const at = (y: number, mo: number, d: number, h: number, mi: number) => Date.UTC(y, mo - 1, d, h, mi) / 1000;
const next = (e: string, t: number, tz?: string) => nextAfter(parseCron(e, tz), t);

describe("cron", () => {
  it("every minute and steps", () => {
    const t = at(2026, 10, 3, 12, 0);
    expect(next("* * * * *", t)).toBe(t + 60);
    expect(next("* * * * *", t + 30)).toBe(t + 60);
    expect(next("*/15 * * * *", t)).toBe(at(2026, 10, 3, 12, 15));
    expect(next("*/15 * * * *", at(2026, 10, 3, 12, 50))).toBe(at(2026, 10, 3, 13, 0));
    expect(next("5/20 * * * *", t)).toBe(at(2026, 10, 3, 12, 5));
    expect(next("5/20 * * * *", at(2026, 10, 3, 12, 45))).toBe(at(2026, 10, 3, 13, 5));
    expect(next("10-20/5 * * * *", at(2026, 10, 3, 12, 16))).toBe(at(2026, 10, 3, 12, 20));
    expect(next("0,30 */6 * * *", t)).toBe(at(2026, 10, 3, 12, 30));
    expect(next("0,30 */6 * * *", at(2026, 10, 3, 12, 30))).toBe(at(2026, 10, 3, 18, 0));
    expect(next("59 23 * * *", at(2026, 12, 31, 23, 59))).toBe(at(2027, 1, 1, 23, 59));
  });

  it("aliases", () => {
    const t = at(2026, 10, 3, 12, 34);
    expect(next("@hourly", t)).toBe(at(2026, 10, 3, 13, 0));
    expect(next("@daily", t)).toBe(at(2026, 10, 4, 0, 0));
    expect(next("@weekly", t)).toBe(at(2026, 10, 4, 0, 0));
    expect(next("@monthly", t)).toBe(at(2026, 11, 1, 0, 0));
    expect(next("@yearly", t)).toBe(at(2027, 1, 1, 0, 0));
    expect(() => parseCron("@reboot")).toThrow();
  });

  it("month ends and leap years", () => {
    expect(next("0 0 31 * *", at(2026, 4, 1, 0, 0))).toBe(at(2026, 5, 31, 0, 0));
    expect(next("0 12 29 2 *", at(2026, 3, 1, 0, 0))).toBe(at(2028, 2, 29, 12, 0));
    expect(next("0 0 29 2 *", at(2096, 3, 1, 0, 0))).toBe(at(2104, 2, 29, 0, 0));
    expect(() => parseCron("0 0 30 2 *")).toThrow(/never fires/);
    expect(() => parseCron("0 0 31 4,6,9,11 *")).toThrow();
  });

  it("days of week and names", () => {
    const sat = at(2026, 10, 3, 9, 0);
    expect(next("0 9 * * mon-fri", sat)).toBe(at(2026, 10, 5, 9, 0));
    expect(next("0 9 * * 7", sat)).toBe(at(2026, 10, 4, 9, 0));
    expect(next("0 9 * * SUN", sat)).toBe(at(2026, 10, 4, 9, 0));
    expect(next("0 0 1 JAN-mar *", sat)).toBe(at(2027, 1, 1, 0, 0));
    expect(next("0 0 13 * 5", sat)).toBe(at(2026, 10, 9, 0, 0));
    expect(next("0 0 13 * 5", at(2026, 10, 9, 0, 0))).toBe(at(2026, 10, 13, 0, 0));
    expect(next("0 0 * * 5", at(2026, 10, 9, 0, 0))).toBe(at(2026, 10, 16, 0, 0));
  });

  it("refuses what the daemon refuses", () => {
    for (const bad of ["", "* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 8", "*/0 * * * *", "5-1 * * * *", "a * * * *", "1,,2 * * * *", "* * * foo *"]) {
      expect(() => parseCron(bad), bad).toThrow();
    }
    expect(() => parseCron("61 * * * *")).toThrow(/minute/);
  });

  it("fixed offsets", () => {
    expect(parseOffset("UTC")).toBe(0);
    expect(parseOffset("+02:00")).toBe(7200);
    expect(parseOffset("-0830")).toBe(-30_600);
    expect(parseOffset("UTC+5")).toBe(18_000);
    expect(() => parseOffset("Europe/Berlin")).toThrow();
    expect(() => parseOffset("+25:00")).toThrow();
    expect(next("0 9 * * *", at(2026, 10, 3, 0, 0), "+02:00")).toBe(at(2026, 10, 3, 7, 0));
    expect(next("0 1 * * *", at(2026, 10, 3, 0, 0), "+02:00")).toBe(at(2026, 10, 3, 23, 0));
    expect(next("30 0 * * 0", at(2026, 10, 3, 0, 0), "-01:00")).toBe(at(2026, 10, 4, 1, 30));
  });

  it("previews the next runs and describes common shapes", () => {
    const t = at(2026, 10, 3, 12, 0);
    expect(nextRuns(parseCron("*/20 * * * *"), t, 3)).toEqual([at(2026, 10, 3, 12, 20), at(2026, 10, 3, 12, 40), at(2026, 10, 3, 13, 0)]);
    const p = cronPreview("0 3 * * *", null, t, 2);
    expect(p).toEqual({ ok: true, runs: [at(2026, 10, 4, 3, 0), at(2026, 10, 5, 3, 0)], text: "Every day at 03:00" });
    expect(cronPreview("nope").ok).toBe(false);
    expect(describeCron("*/5 * * * *")).toBe("Every 5 minutes");
    expect(describeCron("0 9 * * 1-5")).toBe("Weekdays at 09:00");
    expect(describeCron("15 2 1 * *")).toBe("Monthly on day 1 at 02:15");
    expect(describeCron("1-5 * 3 * *")).toBe("1-5 * 3 * *");
    expect(formatRun(at(2026, 10, 4, 3, 0))).toBe("2026-10-04 03:00 UTC");
    expect(formatRun(at(2026, 10, 4, 3, 0), 7200)).toBe("2026-10-04 05:00 UTC+02:00");
  });
});
