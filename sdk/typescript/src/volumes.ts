import { type CallOptions, clientOf } from "./client.js";
import type { ProgressHandler } from "./sandbox.js";
import type { PruneItem, VolumeCreated, VolumeInfo } from "./types.js";

export interface PoolOptions extends CallOptions {
  /** Storage pool. Default `auto`: incus-zfs, else default, else the first pool. */
  pool?: string;
}

function pool(o: PoolOptions): Record<string, unknown> {
  return o.pool === undefined ? {} : { pool: o.pool };
}

/** Named custom storage volumes. */
export const volumes = {
  /** Volumes in `pool`, or in all pools when omitted. */
  list(opts: PoolOptions = {}): Promise<VolumeInfo[]> {
    return clientOf(opts).request<VolumeInfo[]>("volume.list", pool(opts));
  },

  /** One volume. Fails with NotFoundError. */
  get(name: string, opts: PoolOptions = {}): Promise<VolumeInfo> {
    return clientOf(opts).request<VolumeInfo>("volume.get", { name, ...pool(opts) });
  },

  /** Create a volume; a no-op (`created: false`) if it exists. */
  create(
    name: string,
    opts: PoolOptions & { config?: Record<string, string> } = {},
  ): Promise<VolumeCreated> {
    const p: Record<string, unknown> = { name, ...pool(opts) };
    if (opts.config) p.config = opts.config;
    return clientOf(opts).request<VolumeCreated>("volume.create", p);
  },

  /** Delete a volume; refused while in use. */
  async remove(name: string, opts: PoolOptions = {}): Promise<void> {
    await clientOf(opts).request("volume.remove", { name, ...pool(opts) });
  },
};

export interface PruneOptions extends CallOptions {
  /** Only report (default true). Pass false to delete. */
  dryRun?: boolean;
  onProgress?: ProgressHandler;
}

/**
 * Sandboxes whose `label` value is an absolute host path that no longer
 * exists; deleted unless `dryRun` (the default).
 */
export function prune(label: string, opts: PruneOptions = {}): Promise<PruneItem[]> {
  const cb = opts.onProgress;
  return clientOf(opts).request<PruneItem[]>(
    "prune",
    { label, dry_run: opts.dryRun ?? true },
    cb ? (e, d) => e === "progress" && cb(String(d)) : undefined,
  );
}
