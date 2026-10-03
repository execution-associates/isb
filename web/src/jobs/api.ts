// Scheduled jobs (docs/jobs.md). Shapes from src/jobs/mod.rs and
// src/daemon/data.rs.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { Run } from "@/data/api";

export type JobTarget = { app: string } | { stack: string; service: string };

export interface JobSpec {
  name: string;
  schedule: string;
  timezone?: string;
  target: JobTarget;
  mode: "exec" | "run";
  command: string[];
  timeout: string;
  concurrency: "skip" | "allow";
  keep: number;
  enabled: boolean;
  user?: string;
  cwd?: string;
  env?: Record<string, string>;
  missed_grace?: string;
}

export interface JobEntry {
  job: JobSpec;
  created_at: number;
  updated_at: number;
  /** RFC 3339, null when disabled. */
  next_run: string | null;
  last_run: Run | null;
}

export const jkeys = {
  jobs: (org: string) => ["apps", org, "jobs"] as const,
  runs: (org: string, name: string) => ["apps", org, "job-runs", name] as const,
};

export const jobApp = (j: JobSpec) => ("app" in j.target ? j.target.app : `${j.target.stack}/${j.target.service}`);

export function useJobs(org: string) {
  return useQuery({
    queryKey: jkeys.jobs(org),
    queryFn: () => callTool<{ jobs: JobEntry[] }>("job_list", {}, org).then((r) => r.jobs),
  });
}

export function useJobRuns(org: string, name: string | null, refetchInterval?: number | false) {
  return useQuery({
    queryKey: jkeys.runs(org, name ?? ""),
    enabled: !!name,
    refetchInterval,
    queryFn: () => callTool<{ runs: Run[] }>("job_runs", { name: name ?? "", limit: 50 }, org).then((r) => r.runs),
  });
}

/**
 * A command line split as the daemon splits one (src/flex.rs split_words):
 * whitespace separates, single quotes are literal, double quotes allow \" and
 * \\, a backslash outside quotes escapes the next character.
 */
export function splitWords(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let any = false;
  let i = 0;
  const s = line;
  while (i < s.length) {
    const c = s[i];
    if (/\s/.test(c)) {
      if (any) out.push(cur);
      cur = "";
      any = false;
      i++;
    } else if (c === "'") {
      const j = s.indexOf("'", i + 1);
      if (j < 0) throw new Error("an unclosed '");
      cur += s.slice(i + 1, j);
      any = true;
      i = j + 1;
    } else if (c === '"') {
      i++;
      any = true;
      for (;;) {
        if (i >= s.length) throw new Error('an unclosed "');
        const d = s[i];
        if (d === '"') {
          i++;
          break;
        }
        if (d === "\\") {
          const e = s[i + 1];
          if (e === undefined) throw new Error('an unclosed "');
          if (e === '"' || e === "\\" || e === "$" || e === "`") cur += e;
          else if (e !== "\n") cur += "\\" + e;
          i += 2;
        } else {
          cur += d;
          i++;
        }
      }
    } else if (c === "\\") {
      if (i + 1 >= s.length) throw new Error("a trailing \\");
      if (s[i + 1] !== "\n") cur += s[i + 1];
      any = true;
      i += 2;
    } else {
      cur += c;
      any = true;
      i++;
    }
  }
  if (any) out.push(cur);
  return out;
}

/** argv back to a line that splitWords reads the same. */
export function joinWords(argv: string[]): string {
  return argv.map((a) => (a && /^[A-Za-z0-9_@%+=:,./-]+$/.test(a) ? a : `'${a.replace(/'/g, `'"'"'`)}'`)).join(" ");
}

/** A duration as the daemon reads one (src/flex.rs parse_duration): a number
 * and one unit (ms, s, m, h, d; none is seconds). Seconds, or null. */
export function durationSeconds(s: string): number | null {
  const m = /^(\d+(?:\.\d+)?)\s*(ms|s|sec|secs|m|min|mins|h|d)?$/.exec(s.trim());
  if (!m) return null;
  const n = Number(m[1]);
  const u = m[2] ?? "s";
  return u === "ms" ? n / 1000 : u === "h" ? n * 3600 : u === "d" ? n * 86400 : u.startsWith("m") ? n * 60 : n;
}
