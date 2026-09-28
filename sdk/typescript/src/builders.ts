/**
 * Builders for mounts and port forwards. They return plain objects in the
 * compose format (`VolumeSpec`, `PortSpec`), so they can be mixed freely with
 * hand-written ones.
 */

import type { PortSpec, Scalar, VolumeSpec } from "./spec.js";

/** How a named volume is obtained. */
export const NamedVolumeMode = {
  /** Create the volume if it is missing (the default). */
  EnsureExists: "ensure_exists",
  /** The volume must already exist (`external: true`). */
  Existing: "existing",
} as const;
export type NamedVolumeMode = (typeof NamedVolumeMode)[keyof typeof NamedVolumeMode];

export interface BindOptions {
  readonly?: boolean;
  /** Device name (default: derived from the guest path). */
  device?: string;
  /** Extra disk device properties (`shift`, `propagation`, ...). */
  options?: Record<string, Scalar>;
}

export interface NamedOptions extends BindOptions {
  mode?: NamedVolumeMode;
  /** chown the mount point in the guest: `dev`, `dev:dev`, `1000:1000`. */
  owner?: string | number;
  /** Storage pool (default: the sandbox's root pool). */
  pool?: string;
}

function common(o: BindOptions): Partial<VolumeSpec> {
  const v: Partial<VolumeSpec> = {};
  if (o.readonly !== undefined) v.readonly = o.readonly;
  if (o.device !== undefined) v.device = o.device;
  if (o.options !== undefined) v.options = { ...o.options };
  return v;
}

export const Volume = {
  /** Bind-mount a host path. Relative paths resolve against `baseDir`. */
  bind(hostPath: string, opts: BindOptions = {}): VolumeSpec {
    return { bind: hostPath, ...common(opts) };
  },

  /** Mount a named custom storage volume. */
  named(name: string, opts: NamedOptions = {}): VolumeSpec {
    const v: VolumeSpec = { named: name, ...common(opts) };
    if (opts.mode === NamedVolumeMode.Existing) v.external = true;
    else if (opts.mode !== undefined && opts.mode !== NamedVolumeMode.EnsureExists) {
      throw new TypeError(`unknown NamedVolumeMode ${JSON.stringify(opts.mode)}`);
    }
    if (opts.owner !== undefined) v.owner = opts.owner;
    if (opts.pool !== undefined) v.pool = opts.pool;
    return v;
  },
};

export interface PortOptions {
  /** Device name (default: `port-<bind>-<port>`). */
  name?: string;
  /** Extra proxy device properties (`nat`, `proxy_protocol`, ...). */
  options?: Record<string, Scalar>;
}

export interface HostPortOptions extends PortOptions {
  /** If the listen port is taken, try up to this many following ports. */
  search?: number;
}

function port(
  bind: "host" | "guest",
  listen: string,
  connect: string,
  o: HostPortOptions,
): PortSpec {
  const p: PortSpec = { bind, listen, connect };
  if (o.name !== undefined) p.name = o.name;
  if (o.search !== undefined) p.search = o.search;
  if (o.options !== undefined) p.options = { ...o.options };
  return p;
}

export const PortBinding = {
  /** Listen on the host, connect in the guest (publish a guest port). */
  host(listen: string, connect: string, opts: HostPortOptions = {}): PortSpec {
    return port("host", listen, connect, opts);
  },
  /** Listen in the guest, connect on the host (reach a host service). */
  guest(listen: string, connect: string, opts: PortOptions = {}): PortSpec {
    return port("guest", listen, connect, opts);
  },
};
