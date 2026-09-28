import { findIsb } from "./binary.js";
import {
  ClientClosedError,
  errorFromRpc,
  ProcessExitedError,
  ProtocolError,
  type RpcErrorObject,
} from "./errors.js";
import { type Spawner, spawnTransport, type Transport } from "./transport.js";
import { type Duration, durationParam } from "./util.js";

/** Protocol versions this SDK speaks. */
export const PROTOCOL = 1;

export interface ClientOptions {
  /** Path to the isb binary. Default: `$ISB_BIN`, the platform package, `isb` on PATH. */
  isbBin?: string;
  /** incus unix socket (`isb --socket`). */
  socket?: string;
  /** incus project (`isb --project`). */
  project?: string;
  /** Deadline for creating an instance (`isb --create-timeout`), e.g. `"10m"`. */
  createTimeout?: Duration;
  /** Working directory of the subprocess; relative paths resolve against it. */
  cwd?: string;
  /** Environment of the subprocess (default: this process's). */
  env?: Record<string, string | undefined>;
  /**
   * How to start the subprocess: `"node"` (node:child_process, default, also
   * under Bun) or `"bun"` (Bun.spawn; the event loop then stays alive until
   * {@link Client.close}).
   */
  spawner?: Spawner;
  /** How long to wait for the hello line, in milliseconds. Default 30000. */
  helloTimeoutMs?: number;
}

/** The server's hello line. */
export interface Hello {
  isb: string;
  protocol: number;
}

/** Receives the events (`progress`, `stdout`, `stderr`) of one request. */
export type EventHandler = (event: string, data: unknown) => void;

interface Pending {
  resolve(v: unknown): void;
  reject(e: unknown): void;
  onEvent?: EventHandler;
}

interface Running {
  transport: Transport;
  hello: Hello;
}

/**
 * One long-lived `isb rpc` subprocess, started on the first request.
 * Requests run concurrently; replies and events are matched by id.
 *
 * If the subprocess dies, pending requests fail with
 * {@link ProcessExitedError} and the next request starts a new one.
 * After {@link Client.close}, requests fail with {@link ClientClosedError}.
 */
export class Client {
  readonly options: Readonly<ClientOptions>;
  #running: Running | undefined;
  #starting: Promise<Running> | undefined;
  #pending = new Map<number, Pending>();
  #nextId = 1;
  #closed = false;

  constructor(options: ClientOptions = {}) {
    this.options = { ...options };
  }

  /** The command line used to start the server. */
  argv(): string[] {
    const o = this.options;
    const argv = [findIsb(o.isbBin)];
    if (o.socket) argv.push("--socket", o.socket);
    if (o.project) argv.push("--project", o.project);
    const ct = durationParam(o.createTimeout);
    if (ct) argv.push("--create-timeout", ct);
    argv.push("rpc");
    return argv;
  }

  /** The server's hello, starting it if needed. */
  async hello(): Promise<Hello> {
    return (await this.#ensure()).hello;
  }

  /** pid of the running subprocess, if one is running. */
  get pid(): number | undefined {
    return this.#running?.transport.pid;
  }

  get closed(): boolean {
    return this.#closed;
  }

  /**
   * Send one request and wait for its final reply. `onEvent` receives the
   * events sent before it. Rejects with an {@link IsbError} subclass.
   */
  async request<T = unknown>(
    method: string,
    params?: Record<string, unknown>,
    onEvent?: EventHandler,
  ): Promise<T> {
    return (await this.send<T>(method, params, onEvent)).result;
  }

  /**
   * Send one request and return its id as soon as it is written, with the
   * final reply as `result`. The id names a running exec in `exec.*` calls.
   */
  async send<T = unknown>(
    method: string,
    params?: Record<string, unknown>,
    onEvent?: EventHandler,
  ): Promise<{ id: number; result: Promise<T> }> {
    const r = await this.#ensure();
    const id = this.#nextId++;
    const line = JSON.stringify({ id, method, params: params ?? {} });
    const result = new Promise<T>((resolve, reject) => {
      if (this.#pending.size === 0) r.transport.ref();
      this.#pending.set(id, { resolve: resolve as (v: unknown) => void, reject, onEvent });
      try {
        r.transport.send(line);
      } catch (e) {
        this.#settle(id);
        reject(
          new ProcessExitedError(
            "process_exited",
            `isb rpc is not accepting requests: ${(e as Error).message}`,
          ),
        );
      }
    });
    return { id, result };
  }

  /** `{isb, protocol}` of the server. */
  version(): Promise<Hello> {
    return this.request<Hello>("version");
  }

  /** The JSON Schema of the compose format. */
  schema(): Promise<Record<string, unknown> & { $defs?: Record<string, unknown> }> {
    return this.request("schema");
  }

  /**
   * Close the server's stdin and wait for it to exit. In-flight requests
   * finish first; running execs lose their control connection.
   */
  async close(): Promise<void> {
    this.#closed = true;
    let r = this.#running;
    if (!r && this.#starting) r = await this.#starting.catch(() => undefined);
    if (!r) return;
    r.transport.end();
    const timer = setTimeout(() => r.transport.kill("SIGKILL"), 10_000);
    try {
      await r.transport.exited;
    } finally {
      clearTimeout(timer);
    }
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  #settle(id: number): Pending | undefined {
    const p = this.#pending.get(id);
    if (!p) return undefined;
    this.#pending.delete(id);
    if (this.#pending.size === 0) this.#running?.transport.unref();
    return p;
  }

  #ensure(): Promise<Running> {
    if (this.#closed) {
      return Promise.reject(new ClientClosedError("closed", "the isb client is closed"));
    }
    if (this.#running) return Promise.resolve(this.#running);
    if (!this.#starting) {
      this.#starting = this.#start().finally(() => {
        this.#starting = undefined;
      });
    }
    return this.#starting;
  }

  async #start(): Promise<Running> {
    const argv = this.argv();
    let t: Transport;
    try {
      t = spawnTransport(
        argv,
        { cwd: this.options.cwd, env: this.options.env },
        this.options.spawner,
      );
    } catch (e) {
      throw new ProcessExitedError(
        "process_exited",
        `cannot start ${argv[0]}: ${(e as Error).message}`,
      );
    }
    const died = async (what: string): Promise<never> => {
      const st = await t.exited;
      const how = st.signal ? `signal ${st.signal}` : `status ${st.code}`;
      const err = t.stderr().trim();
      throw new ProcessExitedError(
        "process_exited",
        `${argv[0]} rpc exited (${how}) ${what}${err ? `: ${err}` : ""}`,
        { exit_code: st.code, signal: st.signal, stderr: err },
      );
    };
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(
        () => reject(new ProtocolError("protocol", `no hello from ${argv[0]} rpc`)),
        this.options.helloTimeoutMs ?? 30_000,
      );
    });
    let first: IteratorResult<string>;
    try {
      first = await Promise.race([
        t.lines.next().catch(() => ({ done: true, value: undefined }) as const),
        timeout,
      ]);
      if (first.done) await died("before its hello line");
    } catch (e) {
      t.kill("SIGKILL");
      throw e;
    } finally {
      clearTimeout(timer);
    }
    let hello: Hello;
    try {
      hello = JSON.parse(first.value as string);
    } catch {
      t.kill("SIGKILL");
      throw new ProtocolError("protocol", `bad hello from isb rpc: ${first.value}`);
    }
    if (typeof hello !== "object" || hello === null || hello.protocol !== PROTOCOL) {
      t.kill("SIGKILL");
      throw new ProtocolError(
        "protocol",
        `isb rpc speaks protocol ${JSON.stringify(hello?.protocol)}; this SDK speaks ${PROTOCOL}`,
        { hello },
      );
    }
    const running: Running = { transport: t, hello };
    this.#running = running;
    // Idle until a request is pending, so a finished script can exit.
    t.unref();
    void this.#readLoop(running, died);
    return running;
  }

  async #readLoop(r: Running, died: (what: string) => Promise<never>): Promise<void> {
    // stdout normally closes when the process exits. If something else still
    // holds it open, stop reading shortly after the exit anyway.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const gone = new Promise<IteratorResult<string>>((resolve) => {
      void r.transport.exited.then(() => {
        timer = setTimeout(() => resolve({ done: true, value: undefined }), 1000);
        (timer as { unref?(): void }).unref?.();
      });
    });
    try {
      for (;;) {
        const n = await Promise.race([r.transport.lines.next(), gone]);
        if (n.done) break;
        this.#dispatch(n.value);
      }
    } catch {
      // stdout failed: treat as exit
    } finally {
      clearTimeout(timer);
    }
    if (this.#running === r) this.#running = undefined;
    let err: unknown;
    try {
      await died("with requests pending");
    } catch (e) {
      err = e;
    }
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const p of pending) p.reject(err);
  }

  #dispatch(line: string): void {
    let msg: {
      id?: unknown;
      event?: string;
      data?: unknown;
      result?: unknown;
      error?: RpcErrorObject;
    };
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if (typeof msg.id !== "number") return; // id null: a line we never sent
    if (msg.event !== undefined) {
      const p = this.#pending.get(msg.id);
      try {
        p?.onEvent?.(msg.event, msg.data);
      } catch {
        // a throwing handler must not break the reader
      }
      return;
    }
    const p = this.#settle(msg.id);
    if (!p) return;
    if (msg.error) p.reject(errorFromRpc(msg.error));
    else p.resolve(msg.result ?? null);
  }
}

let defaultClientInstance: Client | undefined;

/** The shared client used when an options object gets no `client`. */
export function defaultClient(): Client {
  if (!defaultClientInstance || defaultClientInstance.closed) {
    defaultClientInstance = new Client();
  }
  return defaultClientInstance;
}

/** Replace the shared default client (for example to set a socket). */
export function setDefaultClient(client: Client | undefined): void {
  defaultClientInstance = client;
}

/** Options accepted by every call. */
export interface CallOptions {
  /** Client to use. Default: {@link defaultClient}. */
  client?: Client;
}

export function clientOf(opts?: CallOptions): Client {
  return opts?.client ?? defaultClient();
}
