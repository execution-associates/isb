// Named volumes: snapshots, their schedule and hook, backups of a volume and
// staged restores (docs/volumes.md) as typed calls. Result shapes come from
// src/daemon/volumes.rs and src/volume_backup/ (the OpenAPI document types
// arguments only).
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { BackupSpec, Run } from "@/data/api";

export interface VolumeSummary {
  name: string;
  pool: string;
  instances: string[];
  schedule: string | null;
  next_run: string | null;
  /** Set on a staged restore: the volume it restores. */
  restore_of: string | null;
  size: string | null;
}

export interface Snapshot {
  name: string;
  created_at: string;
  description?: string;
  /** auto: scheduled, pruned to keep; manual and other: kept until deleted. */
  kind: "auto" | "manual" | "other";
}

export interface VolumeSettings {
  schedule?: string;
  timezone?: string;
  keep: number;
  enabled: boolean;
  missed_grace?: string;
  hook_timeout?: string;
  hook_required: boolean;
}

export interface StagedRestore {
  volume: string;
  of: string;
  from: string;
  stamp: string;
  by: string;
  instance: string | null;
  path: string;
  attached: boolean;
}

export interface VolumeBackupEntry {
  backup: BackupSpec;
  last_run: Run | null;
  next_run: string | null;
}

export interface VolumeDetail {
  volume: { name: string; pool: string; config: Record<string, string>; instances: { name: string; running: boolean }[] };
  settings: VolumeSettings;
  next_run: string | null;
  last_run: Run | null;
  snapshots: Snapshot[];
  restores: StagedRestore[];
  backups: VolumeBackupEntry[];
}

export const vkeys = {
  all: (org: string) => ["apps", org, "volumes"] as const,
  volume: (org: string, name: string) => ["apps", org, "volume", name] as const,
  runs: (org: string, name: string) => ["apps", org, "volume-runs", name] as const,
};

export function useVolumes(org: string) {
  return useQuery({
    queryKey: vkeys.all(org),
    queryFn: () => callTool<{ volumes: VolumeSummary[] }>("volume_list", {}, org).then((r) => r.volumes),
  });
}

export function useVolume(org: string, name: string, refetchInterval?: number) {
  return useQuery({
    queryKey: vkeys.volume(org, name),
    refetchInterval,
    queryFn: () => callTool<VolumeDetail>("volume_get", { name }, org),
  });
}

export function useSnapshotRuns(org: string, name: string, refetchInterval?: number) {
  return useQuery({
    queryKey: vkeys.runs(org, name),
    refetchInterval,
    queryFn: () => callTool<{ runs: Run[] }>("volume_snapshot_runs", { name, limit: 30 }, org).then((r) => r.runs),
  });
}

/** `20261003T090912Z` as a Date. */
export function stampDate(stamp: string): Date | null {
  const m = /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})Z$/.exec(stamp);
  return m ? new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6])) : null;
}

/** What a staged restore came from, for people: `snapshot:x` / `backup:key`. */
export function originLabel(from: string): string {
  if (from.startsWith("snapshot:")) return `snapshot ${from.slice(9)}`;
  if (from.startsWith("backup:")) return `backup ${from.slice(7).split("/").pop() ?? ""}`;
  return from;
}

/** A snapshot name a user gives (src/volume_backup/model.rs). */
export function snapshotNameProblem(s: string): string | null {
  if (!s) return null;
  if (s.length > 63 || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(s)) return "Up to 63 letters, digits, _, - and ., starting with a letter or digit.";
  if (s.startsWith("auto-") || s.startsWith("isb-backup-")) return "auto-* and isb-backup-* are isb's own names.";
  return null;
}

/** A hook timeout: 30s, 5m, 1h (at most an hour). */
export function hookTimeoutProblem(s: string): string | null {
  if (!s.trim()) return null;
  const m = /^(\d+)\s*(s|m|h)$/.exec(s.trim());
  if (!m) return "Like 30s, 5m or 1h.";
  const secs = +m[1] * (m[2] === "s" ? 1 : m[2] === "m" ? 60 : 3600);
  if (secs < 1 || secs > 3600) return "More than 0s, at most 1h.";
  return null;
}
