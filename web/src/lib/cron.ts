// Cron schedules as the daemon reads them (src/cron.rs), for previews in
// forms: the next runs and a sentence. The server is the authority; this
// mirrors its parser so a schedule it refuses is flagged before saving.
//
// Five fields (minute hour day-of-month month day-of-week) or an alias;
// `*`, numbers, ranges, steps (`*/n`, `a-b/n`, `a/n`), lists, month and
// weekday names; day of week 0-7 (0 and 7 Sunday); when both day fields are
// restricted a day matching either fires. UTC or a fixed offset, no DST.

export interface Schedule {
  minutes: Set<number>;
  hours: Set<number>;
  doms: Set<number>;
  months: Set<number>;
  dows: Set<number>;
  domStar: boolean;
  dowStar: boolean;
  /** Seconds east of UTC. */
  offset: number;
  text: string;
}

const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

export const ALIASES: Record<string, string> = {
  "@yearly": "0 0 1 1 *",
  "@annually": "0 0 1 1 *",
  "@monthly": "0 0 1 * *",
  "@weekly": "0 0 * * 0",
  "@daily": "0 0 * * *",
  "@midnight": "0 0 * * *",
  "@hourly": "0 * * * *",
};

export class CronError extends Error {}

function field(s: string, lo: number, hi: number, names: string[], what: string): { set: Set<number>; star: boolean } {
  const bad = (why: string) => new CronError(`${what} "${s}": ${why}`);
  const num = (t: string): number => {
    if (/^\d+$/.test(t)) return Number(t);
    const i = names.indexOf(t.toLowerCase());
    if (i < 0) throw bad(`"${t}" is not a number${names.length ? " or a name" : ""}`);
    return i + (names.length === 12 ? 1 : 0);
  };
  const set = new Set<number>();
  let star = false;
  for (const part of s.split(",")) {
    if (!part) throw bad("empty list item");
    let range = part;
    let step: number | null = null;
    const slash = part.indexOf("/");
    if (slash >= 0) {
      range = part.slice(0, slash);
      const st = part.slice(slash + 1);
      if (!/^\d+$/.test(st)) throw bad(`step "${st}" is not a number`);
      step = Number(st);
      if (step === 0) throw bad("a step of 0");
    }
    let a: number;
    let b: number;
    if (range === "*") {
      if ((step ?? 1) === 1) star = true;
      [a, b] = [lo, hi];
    } else if (range.includes("-")) {
      const [x, y] = range.split("-", 2);
      [a, b] = [num(x), num(y)];
    } else {
      a = num(range);
      b = step !== null ? hi : a;
    }
    if (a < lo || b > hi || a > b) throw bad(`${a}-${b} is outside ${lo}-${hi}`);
    for (let v = a; v <= b; v += step ?? 1) set.add(v);
  }
  return { set, star };
}

/** `UTC`, `Z`, `+05:30`, `-0800`, `+2` -> seconds east of UTC. */
export function parseOffset(s: string | undefined | null): number {
  let t = (s ?? "").trim();
  if (!t || t.toLowerCase() === "utc" || t === "Z") return 0;
  const bad = () => new CronError(`timezone "${s}": UTC or a fixed offset such as +02:00 (named zones are not supported)`);
  if (t.startsWith("UTC") || t.startsWith("utc")) t = t.slice(3);
  const sign = t[0] === "+" ? 1 : t[0] === "-" ? -1 : 0;
  if (!sign) throw bad();
  const rest = t.slice(1);
  let h: string;
  let m: string;
  if (rest.includes(":")) [h, m] = rest.split(":", 2);
  else if (rest.length === 4) [h, m] = [rest.slice(0, 2), rest.slice(2)];
  else [h, m] = [rest, "0"];
  if (!/^\d+$/.test(h) || !/^\d+$/.test(m)) throw bad();
  const hh = Number(h);
  const mm = Number(m);
  if (hh > 14 || mm > 59) throw bad();
  return sign * (hh * 3600 + mm * 60);
}

/** Parse a schedule (throws CronError with the reason). */
export function parseCron(expr: string, timezone?: string | null): Schedule {
  const text = expr.trim();
  const lower = text.toLowerCase();
  let expanded = text;
  if (lower.startsWith("@")) {
    expanded = ALIASES[lower];
    if (!expanded) throw new CronError(`"${text}": aliases are @yearly, @monthly, @weekly, @daily and @hourly`);
  }
  const f = expanded.split(/\s+/).filter(Boolean);
  if (f.length !== 5) throw new CronError("Five fields (minute hour day-of-month month day-of-week), or an alias such as @daily.");
  const minutes = field(f[0], 0, 59, [], "minute");
  const hours = field(f[1], 0, 23, [], "hour");
  const doms = field(f[2], 1, 31, [], "day of month");
  const months = field(f[3], 1, 12, MONTHS, "month");
  const dows = field(f[4], 0, 7, DAYS, "day of week");
  if (dows.set.has(7)) {
    dows.set.delete(7);
    dows.set.add(0);
  }
  const s: Schedule = {
    minutes: minutes.set,
    hours: hours.set,
    doms: doms.set,
    months: months.set,
    dows: dows.set,
    domStar: doms.star,
    dowStar: dows.star,
    offset: parseOffset(timezone),
    text,
  };
  if (nextAfter(s, 946_684_800) === null) throw new CronError(`"${text}" never fires.`);
  return s;
}

function dayMatches(s: Schedule, d: Date): boolean {
  if (!s.months.has(d.getUTCMonth() + 1)) return false;
  const dom = s.doms.has(d.getUTCDate());
  const dow = s.dows.has(d.getUTCDay());
  if (s.domStar && s.dowStar) return true;
  if (!s.domStar && s.dowStar) return dom;
  if (s.domStar && !s.dowStar) return dow;
  return dom || dow;
}

/** The first firing strictly after `t` (unix seconds), or null within eight years. */
export function nextAfter(s: Schedule, t: number): number | null {
  const local = t + s.offset;
  const start = Math.floor(local / 60) * 60 + 60;
  const firstDay = Math.floor(start / 86_400);
  for (let day = firstDay; day < firstDay + 366 * 8; day++) {
    const d = new Date(day * 86_400_000);
    if (!dayMatches(s, d)) continue;
    const from = day === firstDay ? Math.floor((start - day * 86_400) / 60) : 0;
    for (let mm = from; mm < 1440; mm++) {
      if (s.hours.has(Math.floor(mm / 60)) && s.minutes.has(mm % 60)) return day * 86_400 + mm * 60 - s.offset;
    }
  }
  return null;
}

/** The next `n` runs after `t` (unix seconds). */
export function nextRuns(s: Schedule, t: number, n: number): number[] {
  const out: number[] = [];
  let at = t;
  while (out.length < n) {
    const x = nextAfter(s, at);
    if (x === null) break;
    out.push(x);
    at = x;
  }
  return out;
}

/** Parse and preview: the next runs or the error, for forms. */
export function cronPreview(
  expr: string,
  timezone?: string | null,
  now = Date.now() / 1000,
  n = 3,
): { ok: true; runs: number[]; text: string } | { ok: false; error: string } {
  try {
    const s = parseCron(expr, timezone);
    return { ok: true, runs: nextRuns(s, now, n), text: describeCron(expr) };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

const pad = (n: number) => String(n).padStart(2, "0");
const DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/** A sentence for the common shapes; the expression itself otherwise. */
export function describeCron(expr: string): string {
  const lower = expr.trim().toLowerCase();
  const alias: Record<string, string> = {
    "@yearly": "Every year on 1 January at 00:00",
    "@annually": "Every year on 1 January at 00:00",
    "@monthly": "On the 1st of every month at 00:00",
    "@weekly": "Every Sunday at 00:00",
    "@daily": "Every day at 00:00",
    "@midnight": "Every day at 00:00",
    "@hourly": "Every hour, on the hour",
  };
  if (alias[lower]) return alias[lower];
  const f = lower.split(/\s+/);
  if (f.length !== 5) return expr;
  const [mi, h, dom, mon, dow] = f;
  const isNum = (x: string) => /^\d+$/.test(x);
  const rest = dom === "*" && mon === "*";
  if (mi === "*" && h === "*" && rest && dow === "*") return "Every minute";
  let m = /^\*\/(\d+)$/.exec(mi);
  if (m && h === "*" && rest && dow === "*") return `Every ${m[1]} minutes`;
  if (isNum(mi) && h === "*" && rest && dow === "*") return mi === "0" ? "Every hour, on the hour" : `Every hour at minute ${Number(mi)}`;
  m = /^\*\/(\d+)$/.exec(h);
  if (isNum(mi) && m && rest && dow === "*") return `Every ${m[1]} hours at minute ${Number(mi)}`;
  if (isNum(mi) && isNum(h)) {
    const at = `${pad(Number(h))}:${pad(Number(mi))}`;
    if (rest && dow === "*") return `Every day at ${at}`;
    if (rest && (dow === "1-5" || dow === "mon-fri")) return `Weekdays at ${at}`;
    if (rest && isNum(dow)) return `Every ${DAY_NAMES[Number(dow) % 7]} at ${at}`;
    if (isNum(dom) && mon === "*" && dow === "*") return `Monthly on day ${Number(dom)} at ${at}`;
  }
  return expr;
}

/** `2026-10-04 03:00 UTC` (or with the schedule's offset). */
export function formatRun(t: number, offset = 0): string {
  const d = new Date((t + offset) * 1000);
  const z = offset === 0 ? "UTC" : `UTC${offset > 0 ? "+" : "-"}${pad(Math.floor(Math.abs(offset) / 3600))}:${pad(Math.floor((Math.abs(offset) % 3600) / 60))}`;
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} ${z}`;
}

export const CRON_PRESETS: { label: string; value: string }[] = [
  { label: "Every minute", value: "* * * * *" },
  { label: "Every 15 minutes", value: "*/15 * * * *" },
  { label: "Hourly", value: "@hourly" },
  { label: "Daily at 03:00", value: "0 3 * * *" },
  { label: "Weekly (Sunday)", value: "@weekly" },
  { label: "Monthly", value: "@monthly" },
];
