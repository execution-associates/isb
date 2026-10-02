/** Result types of the rpc methods (docs/rpc.md). Field names are as sent. */

/** Device or config properties: strings only. */
export type Props = Record<string, string>;

/**
 * Exec defaults as `sandbox.exec` (`defaults`) and `sandbox.wait_ready`
 * (`exec`) take them. A spec implies them through its `user`, `working_dir`
 * and `exec` keys.
 */
export interface ExecDefaults {
  /** Guest user: a name (`dev`), `uid`, or `uid:gid`. */
  user?: string;
  /** Working directory in the guest. */
  cwd?: string;
  /** Environment for exec, over the instance `environment`. */
  env?: Record<string, string>;
  /** Run through the user's login shell. */
  login?: boolean | string;
}

/** A sandbox as listed: `sandbox.get`, `sandbox.list`, `sandbox.create`. */
export interface SandboxInfo {
  name: string;
  /** incus status: `Running`, `Stopped`, ... */
  status: string;
  /** `container` or `virtual-machine`. */
  type: string;
  /** `user.*` config keys without the prefix (isb's own `user.isb.*` excluded). */
  labels: Record<string, string>;
  config: Record<string, string>;
  /** Instance-local devices, by name. */
  devices: Record<string, Props>;
  profiles: string[];
  created_at: string;
  description: string;
}

/** One step of a plan, tagged by `action`. */
export type Action =
  | { action: "create_volume"; pool: string; volume: string; config: Props }
  | {
      action: "create_instance";
      image: string;
      pool: string;
      config: Props;
      devices: Record<string, Props>;
      profiles: string[];
    }
  | { action: "set_config"; key: string; from?: string; to: string; restart: boolean }
  | { action: "add_device"; device: string; props: Props }
  | { action: "replace_device"; device: string; replaces: string; from: Props; to: Props }
  | { action: "remove_device"; device: string; props: Props }
  | { action: "start_instance" }
  | { action: "add_port"; device: string; props: Props; search: number }
  | { action: "fix_owner"; path: string; owner: string }
  | { action: "note"; message: string };

export type ActionKind = Action["action"];

/** What `sandbox.plan` / `compose.plan` would do for one sandbox. */
export interface Plan {
  name: string;
  /** Current status, or null if the sandbox does not exist. */
  status: string | null;
  actions: Action[];
}

/** What an ensure (`connectOrCreate`, `up`) did. */
export interface ApplyReport {
  name: string;
  created: boolean;
  applied: Action[];
  /** Device name to the listen address in use, for searched ports. */
  ports: Record<string, string>;
  /** Config keys changed that take effect after a restart. */
  restart_needed: string[];
}

/** One entry of `compose.up`. */
export interface ServiceReport {
  service: string;
  report: ApplyReport;
}

/** A named custom storage volume. */
export interface VolumeInfo {
  name: string;
  pool: string;
  content_type: string;
  config: Props;
  /** Instances (and profiles) using it, as incus URLs. */
  used_by: string[];
}

/** Result of `volume.create`. */
export interface VolumeCreated {
  /** False when it already existed. */
  created: boolean;
  pool: string;
}

/** One sandbox found by `prune`. */
export interface PruneItem {
  name: string;
  path: string;
  deleted: boolean;
}

/** True when a plan has changes (notes do not count). */
export function planHasChanges(plan: Plan): boolean {
  return plan.actions.some((a) => a.action !== "note");
}
