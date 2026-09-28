import type { Client } from "./client.js";
import type { ExecDefaults } from "./spec.js";
import { b64decode, b64encode, type Duration, durationParam, utf8 } from "./util.js";

/** Options for {@link Sandbox.exec}. They override the sandbox's exec defaults. */
export interface ExecOptions {
  /** Working directory in the guest. */
  cwd?: string;
  /** Guest user: a name, `uid`, or `uid:gid`. */
  user?: string | number;
  /** Environment, over the instance `env` and the exec defaults. */
  env?: Record<string, string>;
  /** Run through the user's login shell. */
  login?: boolean;
  /** Kill the command after this long (seconds, or `"90s"`, `"5m"`). */
  timeout?: Duration;
  /** Data for stdin. Default: no stdin. */
  stdin?: string | Uint8Array;
  /** Pseudo-terminal: output then all arrives as stdout. */
  tty?: boolean;
  /** tty size (default 80x24). */
  width?: number;
  height?: number;
}

/** Options for {@link Sandbox.execStream}. */
export interface ExecStreamOptions extends Omit<ExecOptions, "stdin"> {
  /**
   * `"piped"`: write it with {@link ExecProcess.write} and close it with
   * {@link ExecProcess.closeStdin}. A string or bytes: sent as all of stdin.
   * Default: no stdin.
   */
  stdin?: "piped" | string | Uint8Array;
}

/** Output of a finished command. */
export class ExecOutput {
  constructor(
    readonly exitCode: number,
    readonly stdout: Uint8Array,
    readonly stderr: Uint8Array,
  ) {}

  /** stdout decoded as UTF-8. */
  get stdoutText(): string {
    return utf8(this.stdout);
  }

  /** stderr decoded as UTF-8. */
  get stderrText(): string {
    return utf8(this.stderr);
  }

  /** exit code 0. */
  get success(): boolean {
    return this.exitCode === 0;
  }
}

/** A chunk of output from a streaming exec. */
export interface ExecEvent {
  kind: "stdout" | "stderr";
  data: Uint8Array;
}

export type Argv = string | string[];

/** `cmd` alone, `cmd` plus `args`, or an argv array. */
export function toArgv(cmd: Argv, args?: string[]): string[] {
  const head = Array.isArray(cmd) ? [...cmd] : [cmd];
  const argv = args ? [...head, ...args] : head;
  if (argv.length === 0) throw new TypeError("exec needs a command");
  return argv.map(String);
}

export function execParams(
  name: string,
  argv: string[],
  defaults: ExecDefaults | undefined,
  o: ExecStreamOptions,
  stream: boolean,
): Record<string, unknown> {
  const p: Record<string, unknown> = { name, argv };
  if (defaults && Object.keys(defaults).length > 0) p.defaults = defaults;
  if (o.cwd !== undefined) p.cwd = o.cwd;
  if (o.user !== undefined) p.user = String(o.user);
  if (o.env !== undefined) p.env = o.env;
  if (o.login !== undefined) p.login = o.login;
  if (o.tty) p.tty = true;
  if (o.width !== undefined) p.width = o.width;
  if (o.height !== undefined) p.height = o.height;
  const timeout = durationParam(o.timeout);
  if (timeout !== undefined) p.timeout = timeout;
  if (o.stdin === "piped") p.stdin = "piped";
  else if (o.stdin !== undefined) p.stdin = { data: b64encode(o.stdin) };
  if (stream) p.stream = true;
  return p;
}

const SIGNALS: Record<string, number> = {
  SIGHUP: 1,
  SIGINT: 2,
  SIGQUIT: 3,
  SIGKILL: 9,
  SIGUSR1: 10,
  SIGUSR2: 12,
  SIGTERM: 15,
  SIGCONT: 18,
  SIGSTOP: 19,
  SIGWINCH: 28,
};

function signalNumber(sig: number | string): number {
  if (typeof sig === "number") return sig;
  const n =
    SIGNALS[sig.toUpperCase().startsWith("SIG") ? sig.toUpperCase() : `SIG${sig.toUpperCase()}`];
  if (n === undefined) throw new TypeError(`unknown signal ${sig}; pass its number`);
  return n;
}

type Item = { event: ExecEvent } | { end: true };

/**
 * A running command. Iterate it for output events (one consumer); drive it
 * with {@link write}, {@link closeStdin}, {@link signal}, {@link resize};
 * {@link wait} gives the exit code. Output is buffered until read, so
 * `wait()` alone is fine too.
 */
export class ExecProcess implements AsyncIterable<ExecEvent> {
  readonly #client: Client;
  readonly #id: number;
  readonly #result: Promise<number>;
  #done = false;
  #error: unknown;
  #queue: Item[] = [];
  #waiter: (() => void) | undefined;
  #chain: Promise<unknown> = Promise.resolve();

  /** @internal Use {@link Sandbox.execStream}. */
  static async start(client: Client, params: Record<string, unknown>): Promise<ExecProcess> {
    let proc: ExecProcess | undefined;
    const early: ExecEvent[] = [];
    const onEvent = (event: string, data: unknown) => {
      if (event !== "stdout" && event !== "stderr") return;
      const ev: ExecEvent = { kind: event, data: b64decode(String(data)) };
      if (proc) proc.#push({ event: ev });
      else early.push(ev);
    };
    const { id, result } = await client.send<{ exit_code: number }>(
      "sandbox.exec",
      params,
      onEvent,
    );
    proc = new ExecProcess(client, id, result);
    for (const ev of early) proc.#push({ event: ev });
    return proc;
  }

  private constructor(client: Client, id: number, result: Promise<{ exit_code: number }>) {
    this.#client = client;
    this.#id = id;
    this.#result = result.then(
      (r) => {
        this.#done = true;
        this.#push({ end: true });
        return r.exit_code;
      },
      (e) => {
        this.#done = true;
        this.#error = e;
        this.#push({ end: true });
        throw e;
      },
    );
    // wait() may never be called; do not report an unhandled rejection.
    this.#result.catch(() => {});
  }

  /** The request id of this exec in the protocol. */
  get id(): number {
    return this.#id;
  }

  /** True once the command has finished (or failed). */
  get done(): boolean {
    return this.#done;
  }

  #push(item: Item): void {
    this.#queue.push(item);
    const w = this.#waiter;
    this.#waiter = undefined;
    w?.();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<ExecEvent> {
    for (;;) {
      const item = this.#queue.shift();
      if (!item) {
        await new Promise<void>((r) => {
          this.#waiter = r;
        });
        continue;
      }
      if ("end" in item) {
        if (this.#error) throw this.#error;
        return;
      }
      yield item.event;
    }
  }

  /** The exit code, once the command has finished. Rejects if isb failed (e.g. exec timeout). */
  wait(): Promise<number> {
    return this.#result;
  }

  /**
   * Send an `exec.*` control request. The server queues calls that arrive
   * before the command has started; they are also serialized here so writes
   * stay in order even when not awaited one by one.
   */
  #control(method: string, params: Record<string, unknown>): Promise<void> {
    const run = async () => {
      await this.#client.request(method, { exec: this.#id, ...params });
    };
    const p = this.#chain.then(run, run);
    this.#chain = p.catch(() => {});
    return p;
  }

  /** Write to stdin (`stdin: "piped"` only). */
  write(data: string | Uint8Array): Promise<void> {
    return this.#control("exec.write", { data: b64encode(data) });
  }

  /** Close stdin (EOF). */
  closeStdin(): Promise<void> {
    return this.#control("exec.close_stdin", {});
  }

  /** Send a signal: a number (15) or a name (`"SIGTERM"`, `"TERM"`). */
  signal(sig: number | string): Promise<void> {
    return this.#control("exec.signal", { signal: signalNumber(sig) });
  }

  /** Resize the tty. */
  resize(width: number, height: number): Promise<void> {
    return this.#control("exec.resize", { width, height });
  }

  /** Read all remaining output and wait for the exit code. */
  async output(): Promise<ExecOutput> {
    const out: Uint8Array[] = [];
    const err: Uint8Array[] = [];
    for await (const ev of this) (ev.kind === "stdout" ? out : err).push(ev.data);
    return new ExecOutput(await this.wait(), concat(out), concat(err));
  }
}

function concat(parts: Uint8Array[]): Uint8Array {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

export function execOutputFrom(r: {
  exit_code: number;
  stdout: string;
  stderr: string;
}): ExecOutput {
  return new ExecOutput(r.exit_code, b64decode(r.stdout), b64decode(r.stderr));
}
