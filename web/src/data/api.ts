// Databases, backups and restores (docs/databases.md) as typed calls. Result
// shapes come from src/daemon/data.rs, src/backup.rs and src/jobs/runs.rs
// (the OpenAPI document types arguments only).
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { App } from "@/apps/api";

export type Engine = "postgres" | "mysql" | "mariadb" | "mongodb" | "redis";

export const ENGINES: { id: Engine; label: string; version: string; versions: string[]; port: number }[] = [
  { id: "postgres", label: "PostgreSQL", version: "17", versions: ["17", "16", "15", "14"], port: 5432 },
  { id: "mysql", label: "MySQL", version: "8.4", versions: ["8.4", "8.0", "9"], port: 3306 },
  { id: "mariadb", label: "MariaDB", version: "11.4", versions: ["11.4", "11", "10.11"], port: 3306 },
  { id: "mongodb", label: "MongoDB", version: "8.0", versions: ["8.0", "7.0"], port: 27017 },
  { id: "redis", label: "Redis", version: "7.4", versions: ["7.4", "7.2"], port: 6379 },
];

export const engineInfo = (e: string) => ENGINES.find((x) => x.id === e);
export const engineLabel = (e: string) => engineInfo(e)?.label ?? e;

export interface DatabaseSource {
  engine: Engine;
  version?: string;
  database?: string;
  user?: string;
}

export interface Connection {
  engine: Engine;
  version: string;
  image: string;
  /** The service name inside the org. */
  host: string;
  fqdn: string;
  port: number;
  user?: string;
  database?: string;
  password: { secret: string };
  root_password?: { secret: string };
  /** With the password as a `${{secret...}}` reference. */
  url: string;
  url_secret: string;
  volume: string;
  /** Published URLs (password as a reference). */
  external?: string[];
  /** Only with reveal. */
  password_value?: string;
  url_value?: string;
}

export type Database = App & { source: { database: DatabaseSource }; connection: Connection };

export const isDatabase = (a: { source: unknown }): a is Database =>
  typeof a.source === "object" && a.source !== null && "database" in (a.source as object);

export const dbEnvSnippet = (name: string) => `DATABASE_URL=\${{secret.db.${name}.url}}`;

export type Compression = "gzip" | "zstd" | "none";

export interface Destination {
  name: string;
  endpoint: string;
  region: string;
  bucket: string;
  prefix?: string;
  path_style: boolean;
  access_key_secret: string;
  secret_key_secret: string;
  allow_local?: boolean;
  created_at: number;
}

export interface BackupSpec {
  name: string;
  /** The database app; absent on a volume backup. */
  database?: string;
  /** A named volume (docs/volumes.md). */
  volume?: string;
  destination: string;
  schedule: string;
  timezone?: string;
  keep: number;
  compression: Compression;
  enabled: boolean;
  missed_grace?: string;
}

export type RunStatus = "running" | "succeeded" | "failed" | "skipped";

/** A job, backup or restore run (src/jobs/runs.rs). */
export interface Run {
  id: number;
  kind: "job" | "backup" | "restore";
  trigger: "schedule" | "missed" | "manual";
  by: string;
  scheduled_for?: number;
  status: RunStatus;
  /** Unix milliseconds. */
  started_at: number;
  finished_at?: number;
  duration_ms?: number;
  exit_code?: number;
  error?: string;
  output_bytes: number;
  detail?: {
    key?: string;
    size?: number;
    dump_bytes?: number;
    destination?: string;
    target?: string;
    new?: boolean;
    engine?: string;
    bytes?: number;
    pruned?: string[];
    [k: string]: unknown;
  };
}

export interface BackupFile {
  key: string;
  size: number;
  taken_at: string;
  /** A database dump's engine; absent on a volume backup. */
  engine?: Engine;
  volume?: string;
  compression: Compression;
}

export interface BackupEntry {
  backup: BackupSpec;
  last_run: Run | null;
  /** RFC 3339. */
  next_run: string | null;
  files?: BackupFile[] | { error: string };
}

export interface RunLog {
  text: string;
  offset: number;
  finished: boolean;
  run: Run;
}

export const dkeys = {
  databases: (org: string) => ["apps", org, "databases"] as const,
  database: (org: string, name: string, reveal = false) => ["apps", org, "database", name, reveal] as const,
  destinations: (org: string) => ["apps", org, "backup-destinations"] as const,
  backups: (org: string, database?: string) => ["apps", org, "backups", database ?? "*"] as const,
  backup: (org: string, name: string) => ["apps", org, "backup", name] as const,
  runs: (org: string, name: string) => ["apps", org, "backup-runs", name] as const,
  restores: (org: string) => ["apps", org, "restore-runs"] as const,
};

export function useDatabases(org: string) {
  return useQuery({
    queryKey: dkeys.databases(org),
    queryFn: () => callTool<{ databases: Database[] }>("database_list", {}, org).then((r) => r.databases),
  });
}

export function useDatabase(org: string, name: string) {
  return useQuery({
    queryKey: dkeys.database(org, name),
    queryFn: () => callTool<Database>("database_get", { name }, org),
  });
}

export function useDestinations(org: string) {
  return useQuery({
    queryKey: dkeys.destinations(org),
    queryFn: () => callTool<{ destinations: Destination[] }>("backup_destination_list", {}, org).then((r) => r.destinations),
  });
}

export function useBackups(org: string, database?: string) {
  return useQuery({
    queryKey: dkeys.backups(org, database),
    queryFn: () => callTool<{ backups: BackupEntry[] }>("backup_list", database ? { database } : {}, org).then((r) => r.backups),
  });
}

/** One backup with the files in its bucket (a LIST against S3). */
export function useBackupFiles(org: string, name: string | null) {
  return useQuery({
    queryKey: dkeys.backup(org, name ?? ""),
    enabled: !!name,
    queryFn: () => callTool<{ backups: BackupEntry[] }>("backup_list", { name: name ?? "" }, org).then((r) => r.backups[0]),
    staleTime: 15_000,
  });
}

export function useBackupRuns(org: string, name: string | null, refetchInterval?: number) {
  return useQuery({
    queryKey: dkeys.runs(org, name ?? ""),
    enabled: !!name,
    refetchInterval,
    queryFn: () => callTool<{ runs: Run[] }>("backup_runs", { name: name ?? "", limit: 50 }, org).then((r) => r.runs),
  });
}

export function useRestoreRuns(org: string, refetchInterval?: number) {
  return useQuery({
    queryKey: dkeys.restores(org),
    refetchInterval,
    queryFn: () => callTool<{ runs: Run[] }>("backup_runs", { restores: true, limit: 50 }, org).then((r) => r.runs),
  });
}

/** Database names: a-z, 0-9 and -, like app names. */
export function dbNameProblem(s: string): string | null {
  if (!s) return "Give the database a name.";
  if (s.length > 30 || !/^[a-z][a-z0-9-]*$/.test(s) || s.endsWith("-")) return "Up to 30 characters of a-z, 0-9 and -, starting with a letter.";
  return null;
}

/** A backup or job name (src/jobs/mod.rs validate_name). */
export function scheduleNameProblem(s: string): string | null {
  if (!s) return "Give it a name.";
  if (s.length > 30 || !/^[a-z][a-z0-9-]*$/.test(s) || s.endsWith("-")) return "Up to 30 characters of a-z, 0-9 and -, starting with a letter.";
  return null;
}
