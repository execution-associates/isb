/**
 * Builders for mounts and port forwards. They return plain entries of a
 * service's `volumes` and `ports` lists in the compose format (`VolumeMount`,
 * `PortMapping`, `ProxyPort`), so they can be mixed freely with hand-written
 * ones, including the short string forms.
 */

import type { PortMapping, ProxyPort, Scalar, VolumeMount } from "./spec.js";

export interface BindOptions {
  /** Mount read-only (`read_only`). */
  readOnly?: boolean;
  /** incus device name (default: derived from the target). */
  device?: string;
  /** Extra disk device properties (`shift`, `propagation`, ...). */
  options?: Record<string, Scalar>;
}

export interface NamedOptions extends BindOptions {
  /** The volume must already exist; isb never creates it. */
  external?: boolean;
  /** chown the mount point in the guest: `dev`, `dev:dev`, `1000:1000`. */
  owner?: string | number;
  /** Storage pool (default: the top-level volume's pool, else the sandbox's root pool). */
  pool?: string;
  /** Do not seed an empty volume with what the image has at `target`. */
  nocopy?: boolean;
}

function mount(
  type: "bind" | "volume",
  source: string,
  target: string,
  o: BindOptions,
): VolumeMount {
  const v: VolumeMount = { type, source, target };
  if (o.readOnly !== undefined) v.read_only = o.readOnly;
  if (o.device !== undefined) v.device = o.device;
  if (o.options !== undefined) v.options = { ...o.options };
  return v;
}

export const Volume = {
  /**
   * Bind-mount a host path at `target` (absolute, in the guest). Relative
   * host paths resolve against `baseDir`.
   */
  bind(hostPath: string, target: string, opts: BindOptions = {}): VolumeMount {
    return mount("bind", hostPath, target, opts);
  },

  /**
   * Mount a named custom storage volume at `target`. `name` is the key of a
   * named-volume definition (a compose file's top-level `volumes:`, or the
   * `volumes` option of `Sandbox.create`); without one, it is the incus
   * volume name.
   */
  named(name: string, target: string, opts: NamedOptions = {}): VolumeMount {
    const v = mount("volume", name, target, opts);
    if (opts.external) v.external = true;
    if (opts.owner !== undefined) v.owner = opts.owner;
    if (opts.pool !== undefined) v.pool = opts.pool;
    if (opts.nocopy) v.volume = { nocopy: true };
    return v;
  },
};

export interface PortOptions {
  /** incus device name (default: `port-<bind>-<listen port>`). */
  name?: string;
  /** Extra proxy device properties (`nat`, `proxy_protocol`, ...). */
  options?: Record<string, Scalar>;
}

export interface HostPortOptions extends PortOptions {
  /**
   * If the listen port is taken, try up to this many following ports. Emitted
   * as a published range (`published: "5173-5223"`), so `listen` must be a
   * single TCP/UDP port and `connect` a single port with no host other than
   * the default (`127.0.0.1`, `0.0.0.0`).
   */
  search?: number;
}

export interface PublishOptions extends PortOptions {
  /** Host address to listen on (default `127.0.0.1`). */
  hostIp?: string;
  /** `tcp` (default) or `udp`. */
  protocol?: "tcp" | "udp";
}

const PROTO = /^(tcp|udp)$/;

/** `[proto, host | undefined, port]` of a single-port tcp/udp address, else undefined. */
function splitAddr(addr: string): [string, string | undefined, number] | undefined {
  let a = addr.trim();
  let proto = "tcp";
  const colon = a.indexOf(":");
  const slash = a.lastIndexOf("/");
  if (colon > 0 && PROTO.test(a.slice(0, colon))) {
    proto = a.slice(0, colon);
    a = a.slice(colon + 1);
  } else if (slash >= 0 && PROTO.test(a.slice(slash + 1))) {
    proto = a.slice(slash + 1);
    a = a.slice(0, slash);
  }
  let host: string | undefined;
  let port = a;
  if (a.startsWith("[")) {
    const end = a.indexOf("]");
    if (end < 0 || a[end + 1] !== ":") return undefined;
    host = a.slice(1, end);
    port = a.slice(end + 2);
  } else {
    const i = a.lastIndexOf(":");
    if (i >= 0) {
      host = a.slice(0, i);
      port = a.slice(i + 1);
    }
  }
  if (!/^\d+$/.test(port)) return undefined;
  const n = Number(port);
  if (n < 1 || n > 65535) return undefined;
  return [proto, host, n];
}

function searched(
  listen: string | number,
  connect: string | number,
  o: HostPortOptions,
): PortMapping {
  const search = o.search as number;
  if (!Number.isInteger(search) || search < 0) {
    throw new TypeError(`search must be a whole number, got ${JSON.stringify(search)}`);
  }
  const l = splitAddr(String(listen));
  const c = splitAddr(String(connect));
  const cHost = c?.[1];
  if (
    !l ||
    !c ||
    l[0] !== c[0] ||
    (cHost !== undefined && !["127.0.0.1", "0.0.0.0"].includes(cHost))
  ) {
    throw new TypeError(
      `search needs a single tcp/udp listen port and a connect port on the guest's default address ` +
        `(got listen ${JSON.stringify(listen)}, connect ${JSON.stringify(connect)}); ` +
        `write the port as {published: "A-B", target} instead`,
    );
  }
  const [proto, host, port] = l;
  if (port + search > 65535) {
    throw new TypeError(`search ${search} from port ${port} runs past 65535`);
  }
  const p: PortMapping = {
    published: search > 0 ? `${port}-${port + search}` : String(port),
    target: c[2],
  };
  if (host !== undefined && host !== "127.0.0.1") p.host_ip = host;
  if (proto !== "tcp") p.protocol = proto;
  if (o.name !== undefined) p.name = o.name;
  if (o.options !== undefined) p.options = { ...o.options };
  return p;
}

function proxy(
  bind: "host" | "guest",
  listen: string | number,
  connect: string | number,
  o: PortOptions,
): ProxyPort {
  const p: ProxyPort = { bind, listen: String(listen), connect: String(connect) };
  if (o.name !== undefined) p.name = o.name;
  if (o.options !== undefined) p.options = { ...o.options };
  return p;
}

export const PortBinding = {
  /**
   * Publish a guest port on the host, as docker's `ports:` does: listen on
   * `hostIp` (default 127.0.0.1) at `published`, connect to `target` in the
   * guest. `published` may be a range (`"5173-5223"`): with a single `target`
   * the first free port in it is taken.
   */
  publish(
    published: string | number,
    target: string | number,
    opts: PublishOptions = {},
  ): PortMapping {
    const p: PortMapping = { published, target };
    if (opts.hostIp !== undefined) p.host_ip = opts.hostIp;
    if (opts.protocol !== undefined) p.protocol = opts.protocol;
    if (opts.name !== undefined) p.name = opts.name;
    if (opts.options !== undefined) p.options = { ...opts.options };
    return p;
  },

  /**
   * Listen on the host, connect in the guest (publish a guest port), with
   * incus addresses: `5173`, `"0.0.0.0:5173"`, `"5353/udp"`, or the full
   * `"tcp:HOST:PORT"`; the protocol defaults to tcp and the host to 127.0.0.1.
   * With `search`, the result is a published range (see {@link HostPortOptions}).
   */
  host(
    listen: string | number,
    connect: string | number,
    opts: HostPortOptions = {},
  ): ProxyPort | PortMapping {
    if (opts.search !== undefined) return searched(listen, connect, opts);
    return proxy("host", listen, connect, opts);
  },

  /** Listen in the guest, connect on the host (reach a host service). */
  guest(listen: string | number, connect: string | number, opts: PortOptions = {}): ProxyPort {
    return proxy("guest", listen, connect, opts);
  },
};
