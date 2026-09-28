import { resolve } from "node:path";
import { type CallOptions, type Client, clientOf } from "./client.js";
import { NotFoundError } from "./errors.js";
import { type ProgressHandler, Sandbox } from "./sandbox.js";
import type { ComposeFile, SandboxSpec } from "./spec.js";
import type { Plan, ServiceReport } from "./types.js";

export interface LoadOptions extends CallOptions {
  /** Compose files, merged in order. Default: `isb.yaml` / `isb.yml` in the server's cwd. */
  files?: string[];
  /** dotenv files for `${VAR}` interpolation. */
  envFiles?: string[];
  /** Overrides the file's `name`. */
  projectName?: string;
  /** Variables for interpolation; they win over the environment and env files. */
  vars?: Record<string, string>;
}

export interface UpOptions {
  /** Services to act on (default: all). */
  services?: string[];
  pruneDevices?: boolean;
  /** Run readiness checks (default true). */
  waitReady?: boolean;
  onProgress?: ProgressHandler;
}

export interface ProjectPlanOptions {
  services?: string[];
  pruneDevices?: boolean;
}

export interface DownOptions {
  services?: string[];
  /** Also delete the file's non-external named volumes (all services only). */
  volumes?: boolean;
  onProgress?: ProgressHandler;
}

interface LoadResult {
  name: string;
  base_dir: string;
  files: string[];
  file: ComposeFile;
}

/** A loaded compose file (`isb up/plan/down` over rpc). */
export class Project {
  readonly client: Client;
  /** Project name (sandbox names default to `<name>-<service>`). */
  readonly name: string;
  /** Directory relative bind paths resolve against. */
  readonly baseDir: string;
  /** The files that were merged. */
  readonly files: string[];
  /** The resolved file: interpolated, merged, every sandbox named. */
  readonly file: ComposeFile;
  readonly #load: Record<string, unknown>;

  private constructor(client: Client, r: LoadResult, load: Record<string, unknown>) {
    this.client = client;
    this.name = r.name;
    this.baseDir = r.base_dir;
    this.files = r.files;
    this.file = r.file;
    this.#load = load;
  }

  /** Load (interpolate, merge, validate) compose files. Nothing is touched. */
  static async load(opts: LoadOptions = {}): Promise<Project> {
    const client = clientOf(opts);
    const load: Record<string, unknown> = {};
    // Absolute paths (relative to the server's cwd), so later calls do not
    // depend on where the server runs.
    const cwd = client.options.cwd ?? process.cwd();
    if (opts.files?.length) load.files = opts.files.map((f) => resolve(cwd, f));
    if (opts.envFiles?.length) load.env_files = opts.envFiles.map((f) => resolve(cwd, f));
    if (opts.projectName !== undefined) load.project_name = opts.projectName;
    if (opts.vars !== undefined) load.vars = opts.vars;
    const r = await client.request<LoadResult>("compose.load", load);
    if (!load.files) load.files = r.files;
    return new Project(client, r, load);
  }

  /** Service names, in file order. */
  get services(): string[] {
    return Object.keys(this.file.sandboxes ?? {});
  }

  /** The resolved spec of one service. */
  spec(service: string): SandboxSpec {
    const s = this.file.sandboxes?.[service];
    if (!s) throw new NotFoundError("not_found", `no service ${service} in project ${this.name}`);
    return s;
  }

  /** A handle on a service's sandbox, with that service's exec defaults. */
  sandbox(service: string): Sandbox {
    const s = this.spec(service);
    return Sandbox.fromSpec(s, this.client, s.name as string);
  }

  #params(extra: Record<string, unknown>): Record<string, unknown> {
    return { ...this.#load, ...extra };
  }

  /** Create or reconcile the services' sandboxes. */
  up(opts: UpOptions = {}): Promise<ServiceReport[]> {
    const p: Record<string, unknown> = {};
    if (opts.services) p.services = opts.services;
    if (opts.pruneDevices) p.prune_devices = true;
    if (opts.waitReady !== undefined) p.wait_ready = opts.waitReady;
    const cb = opts.onProgress;
    return this.client.request<ServiceReport[]>(
      "compose.up",
      this.#params(p),
      cb ? (e, d) => e === "progress" && cb(String(d)) : undefined,
    );
  }

  /** What `up` would change, per sandbox. */
  plan(opts: ProjectPlanOptions = {}): Promise<Plan[]> {
    const p: Record<string, unknown> = {};
    if (opts.services) p.services = opts.services;
    if (opts.pruneDevices) p.prune_devices = true;
    return this.client.request<Plan[]>("compose.plan", this.#params(p));
  }

  /** Delete the services' sandboxes (and with `volumes`, the named volumes). */
  async down(opts: DownOptions = {}): Promise<void> {
    const p: Record<string, unknown> = {};
    if (opts.services) p.services = opts.services;
    if (opts.volumes) p.volumes = true;
    const cb = opts.onProgress;
    await this.client.request(
      "compose.down",
      this.#params(p),
      cb ? (e, d) => e === "progress" && cb(String(d)) : undefined,
    );
  }
}
