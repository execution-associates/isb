import { type CallOptions, type Client, clientOf } from "./client.js";
import {
  type Argv,
  type ExecOptions,
  type ExecOutput,
  ExecProcess,
  type ExecStreamOptions,
  execOutputFrom,
  execParams,
  toArgv,
} from "./exec.js";
import type {
  ExecDefaults,
  NamedVolumeSpec,
  PortSpec,
  ReadyCheck,
  SandboxSpec as SpecFields,
} from "./spec.js";
import type { ApplyReport, Plan, SandboxInfo } from "./types.js";
import { type Duration, durationParam } from "./util.js";

/** A sandbox spec with its instance name, as `sandbox.*` methods take it. */
export type SandboxSpec = SpecFields & { name: string; image: string };

/** Receives progress lines (`web: creating from dev-base`). */
export type ProgressHandler = (line: string) => void;

export interface SpecOptions extends CallOptions {
  /** Anchor for relative bind paths (default: the server's working directory). */
  baseDir?: string;
  /** Named-volume definitions, as a compose file's top-level `volumes:`. */
  volumes?: Record<string, NamedVolumeSpec>;
}

export interface CreateOptions extends SpecOptions {
  /** Run the readiness checks (default true). */
  waitReady?: boolean;
  onProgress?: ProgressHandler;
}

export interface EnsureOptions extends CreateOptions {
  /** Remove instance-local devices the spec does not mention. */
  pruneDevices?: boolean;
}

export interface PlanOptions extends SpecOptions {
  pruneDevices?: boolean;
}

export interface ListOptions {
  /**
   * Label filters, all of which must match: `{app: "web", worktree: null}`
   * (null: the key is present, any value), or `["app=web", "worktree"]`.
   */
  labels?: Record<string, string | null> | string[];
}

export interface WaitReadyOptions {
  /** Checks to run. Default: the spec's `ready` if known, else `["running"]`. */
  ready?: ReadyCheck[];
  /** Deadline for all checks. Default: the spec's `ready_timeout` if known, else 60s. */
  readyTimeout?: Duration;
}

export interface StopOptions {
  force?: boolean;
  /** How long to wait for a clean shutdown (default 30s). */
  timeout?: Duration;
}

function specParams(spec: SandboxSpec, o: SpecOptions): Record<string, unknown> {
  const p: Record<string, unknown> = { spec };
  if (o.baseDir !== undefined) p.base_dir = o.baseDir;
  if (o.volumes !== undefined) p.volumes = o.volumes;
  return p;
}

function progress(o: CreateOptions): ((event: string, data: unknown) => void) | undefined {
  const cb = o.onProgress;
  if (!cb) return undefined;
  return (event, data) => {
    if (event === "progress") cb(String(data));
  };
}

function labelFilters(labels: ListOptions["labels"]): string[] {
  if (!labels) return [];
  if (Array.isArray(labels)) return labels;
  return Object.entries(labels).map(([k, v]) => (v === null ? k : `${k}=${v}`));
}

/** What a Sandbox remembers from the spec it was made from. */
export interface SandboxDefaults {
  exec?: ExecDefaults;
  ready?: ReadyCheck[] | null;
  ready_timeout?: string | number | null;
}

/**
 * A handle on one sandbox (an incus instance) by name. Methods always ask
 * isb; nothing is cached except the spec's exec and readiness defaults.
 */
export class Sandbox {
  readonly name: string;
  readonly client: Client;
  /** Exec defaults sent with every exec (from the spec, when known). */
  readonly execDefaults: ExecDefaults | undefined;
  readonly #ready: ReadyCheck[] | undefined;
  readonly #readyTimeout: string | number | undefined;
  /** The report of the last `connectOrCreate` that produced this handle. */
  lastReport: ApplyReport | undefined;

  constructor(name: string, client: Client, defaults: SandboxDefaults = {}) {
    this.name = name;
    this.client = client;
    this.execDefaults = defaults.exec ?? undefined;
    this.#ready = defaults.ready ?? undefined;
    this.#readyTimeout = defaults.ready_timeout ?? undefined;
  }

  /** @internal A handle carrying the spec's exec and readiness defaults. */
  static fromSpec(spec: SpecFields, client: Client, name: string): Sandbox {
    return new Sandbox(name, client, {
      exec: spec.exec,
      ready: spec.ready,
      ready_timeout: spec.ready_timeout,
    });
  }

  /** Create a sandbox. Fails with AlreadyExistsError if it exists. */
  static async create(spec: SandboxSpec, opts: CreateOptions = {}): Promise<Sandbox> {
    const client = clientOf(opts);
    const p = specParams(spec, opts);
    if (opts.waitReady !== undefined) p.wait_ready = opts.waitReady;
    const info = await client.request<SandboxInfo>("sandbox.create", p, progress(opts));
    return Sandbox.fromSpec(spec, client, info.name);
  }

  /**
   * Create the sandbox, or reconcile an existing one to the spec (`isb up`
   * for one sandbox). What was done is in `lastReport`.
   */
  static async connectOrCreate(spec: SandboxSpec, opts: EnsureOptions = {}): Promise<Sandbox> {
    const client = clientOf(opts);
    const p = specParams(spec, opts);
    if (opts.waitReady !== undefined) p.wait_ready = opts.waitReady;
    if (opts.pruneDevices) p.prune_devices = true;
    const r = await client.request<{ info: SandboxInfo; report: ApplyReport }>(
      "sandbox.ensure",
      p,
      progress(opts),
    );
    const sb = Sandbox.fromSpec(spec, client, r.info.name);
    sb.lastReport = r.report;
    return sb;
  }

  /** What `connectOrCreate` would change. */
  static plan(spec: SandboxSpec, opts: PlanOptions = {}): Promise<Plan> {
    const p = specParams(spec, opts);
    if (opts.pruneDevices) p.prune_devices = true;
    return clientOf(opts).request<Plan>("sandbox.plan", p);
  }

  /** The spec resolved against this host (config keys, devices, pool, readiness). */
  static resolve(spec: SandboxSpec, opts: SpecOptions = {}): Promise<Record<string, unknown>> {
    return clientOf(opts).request("sandbox.resolve", specParams(spec, opts));
  }

  /** An existing sandbox. Fails with NotFoundError. */
  static async get(name: string, opts: CallOptions = {}): Promise<Sandbox> {
    const client = clientOf(opts);
    const info = await client.request<SandboxInfo>("sandbox.get", { name });
    return new Sandbox(info.name, client);
  }

  /** Sandboxes matching all label filters. */
  static listWith(filter: ListOptions = {}, opts: CallOptions = {}): Promise<SandboxInfo[]> {
    return clientOf(opts).request<SandboxInfo[]>("sandbox.list", {
      labels: labelFilters(filter.labels),
    });
  }

  /** All sandboxes. */
  static list(opts: CallOptions = {}): Promise<SandboxInfo[]> {
    return Sandbox.listWith({}, opts);
  }

  /** Delete a sandbox. A running one needs `force`. */
  static async remove(name: string, opts: CallOptions & { force?: boolean } = {}): Promise<void> {
    await clientOf(opts).request("sandbox.remove", { name, force: opts.force ?? false });
  }

  info(): Promise<SandboxInfo> {
    return this.client.request<SandboxInfo>("sandbox.get", { name: this.name });
  }

  /** The labels (`user.*` keys without the prefix). */
  async labels(): Promise<Record<string, string>> {
    return (await this.info()).labels;
  }

  /** Start, and wait until it is running. */
  async start(): Promise<void> {
    await this.client.request("sandbox.start", { name: this.name });
  }

  async stop(opts: StopOptions = {}): Promise<void> {
    const p: Record<string, unknown> = { name: this.name, force: opts.force ?? false };
    const t = durationParam(opts.timeout);
    if (t !== undefined) p.timeout = t;
    await this.client.request("sandbox.stop", p);
  }

  async restart(): Promise<void> {
    await this.client.request("sandbox.restart", { name: this.name });
  }

  /** Run readiness checks until they pass. Fails with NotReadyError. */
  async waitReady(opts: WaitReadyOptions = {}): Promise<void> {
    const p: Record<string, unknown> = { name: this.name };
    const ready = opts.ready ?? this.#ready;
    if (ready !== undefined) p.ready = ready;
    const t = durationParam(opts.readyTimeout ?? this.#readyTimeout);
    if (t !== undefined) p.ready_timeout = t;
    if (this.execDefaults) p.exec = this.execDefaults;
    await this.client.request("sandbox.wait_ready", p);
  }

  /** Delete this sandbox. A running one needs `force`. */
  async remove(opts: { force?: boolean } = {}): Promise<void> {
    await this.client.request("sandbox.remove", { name: this.name, force: opts.force ?? false });
  }

  /** Add a proxy device (or leave a correct one alone); returns the listen address in use. */
  async addPort(port: PortSpec): Promise<string> {
    const r = await this.client.request<{ listen: string }>("sandbox.add_port", {
      name: this.name,
      port,
    });
    return r.listen;
  }

  /** Remove an instance-local device. Returns false if it was not there. */
  async removeDevice(device: string): Promise<boolean> {
    const r = await this.client.request<{ removed: boolean }>("sandbox.remove_device", {
      name: this.name,
      device,
    });
    return r.removed;
  }

  /**
   * Run a command and collect its output. `cmd` is the program (then `args`
   * are its arguments) or a whole argv array. Never joined into a shell string.
   */
  async exec(cmd: Argv, args?: string[] | ExecOptions, opts?: ExecOptions): Promise<ExecOutput> {
    const [a, o] = Array.isArray(args) ? [args, opts ?? {}] : [undefined, args ?? opts ?? {}];
    const r = await this.client.request<{ exit_code: number; stdout: string; stderr: string }>(
      "sandbox.exec",
      execParams(this.name, toArgv(cmd, a), this.execDefaults, o, false),
    );
    return execOutputFrom(r);
  }

  /** Start a command and stream its output. */
  execStream(
    cmd: Argv,
    args?: string[] | ExecStreamOptions,
    opts?: ExecStreamOptions,
  ): Promise<ExecProcess> {
    const [a, o] = Array.isArray(args) ? [args, opts ?? {}] : [undefined, args ?? opts ?? {}];
    return ExecProcess.start(
      this.client,
      execParams(this.name, toArgv(cmd, a), this.execDefaults, o, true),
    );
  }
}
