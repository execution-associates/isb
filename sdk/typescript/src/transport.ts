/**
 * The `isb rpc` subprocess, behind a small surface with two implementations:
 * node:child_process (Node >= 20 and Bun) and Bun.spawn.
 */

import { spawn } from "node:child_process";

export interface ExitStatus {
  code: number | null;
  signal: string | null;
}

export interface Transport {
  readonly pid: number | undefined;
  /** stdout, split into lines (without the newline). Ends when stdout closes. */
  readonly lines: AsyncIterator<string>;
  /** Resolves when the process has exited. */
  readonly exited: Promise<ExitStatus>;
  /** Write one line (a newline is appended). Throws if stdin is closed. */
  send(line: string): void;
  /** Close stdin: the server finishes in-flight requests and exits. */
  end(): void;
  kill(signal?: NodeJS.Signals): void;
  /** Let the event loop exit while this process runs (and undo that). */
  ref(): void;
  unref(): void;
  /** The last few KiB of stderr, for error messages. */
  stderr(): string;
}

export interface SpawnOptions {
  cwd?: string;
  env?: Record<string, string | undefined>;
}

const STDERR_TAIL = 8192;

function tail(buf: string, add: string): string {
  const s = buf + add;
  return s.length > STDERR_TAIL ? s.slice(s.length - STDERR_TAIL) : s;
}

async function* splitLines(chunks: AsyncIterable<Uint8Array | string>): AsyncGenerator<string> {
  const dec = new TextDecoder();
  let buf = "";
  for await (const c of chunks) {
    buf += typeof c === "string" ? c : dec.decode(c, { stream: true });
    let i = buf.indexOf("\n");
    while (i >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      if (line.trim()) yield line;
      i = buf.indexOf("\n");
    }
  }
  buf += dec.decode();
  if (buf.trim()) yield buf;
}

/**
 * Which spawner to use. `node` (node:child_process, native in Bun too) is
 * the default: it lets the event loop exit while the idle server runs, so a
 * script that never calls `close()` still ends. `bun` uses Bun.spawn, whose
 * stdout reader keeps the event loop alive until the client is closed.
 */
export type Spawner = "node" | "bun";

export function spawnTransport(
  argv: string[],
  opts: SpawnOptions = {},
  spawner: Spawner = "node",
): Transport {
  const bun = (globalThis as { Bun?: typeof Bun }).Bun;
  if (spawner === "bun") {
    if (!bun) throw new Error('spawner "bun" needs the Bun runtime');
    return spawnBun(bun, argv, opts);
  }
  return spawnNode(argv, opts);
}

function spawnBun(bun: typeof Bun, argv: string[], opts: SpawnOptions): Transport {
  const proc = bun.spawn(argv, {
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
    cwd: opts.cwd,
    env: opts.env as Record<string, string> | undefined,
  });
  let err = "";
  (async () => {
    const dec = new TextDecoder();
    for await (const c of proc.stderr as ReadableStream<Uint8Array>) {
      err = tail(err, dec.decode(c, { stream: true }));
    }
  })().catch(() => {});
  let stdinOpen = true;
  const lines = splitLines(proc.stdout as ReadableStream<Uint8Array>);
  return {
    pid: proc.pid,
    lines,
    exited: proc.exited.then((code) => ({
      code: proc.signalCode ? null : code,
      signal: proc.signalCode ?? null,
    })),
    send(line) {
      if (!stdinOpen) throw new Error("stdin is closed");
      proc.stdin.write(`${line}\n`);
      proc.stdin.flush();
    },
    end() {
      if (!stdinOpen) return;
      stdinOpen = false;
      try {
        proc.stdin.end();
      } catch {
        // already gone
      }
    },
    kill(signal) {
      proc.kill(signal);
    },
    ref() {
      proc.ref();
    },
    unref() {
      proc.unref();
    },
    stderr: () => err,
  };
}

function spawnNode(argv: string[], opts: SpawnOptions): Transport {
  const [cmd, ...args] = argv;
  const child = spawn(cmd as string, args, {
    stdio: ["pipe", "pipe", "pipe"],
    cwd: opts.cwd,
    env: opts.env,
  });
  let err = "";
  let spawnError: Error | undefined;
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (d: string) => {
    err = tail(err, d);
  });
  const exited = new Promise<ExitStatus>((resolve) => {
    child.on("error", (e) => {
      spawnError = e;
      err = tail(err, `${e.message}\n`);
      resolve({ code: null, signal: null });
    });
    // "exit", not "close": a grandchild may hold stdout open after isb is gone.
    child.on("exit", (code, signal) => resolve({ code, signal }));
  });
  child.stdin.on("error", () => {});
  let stdinOpen = true;
  const handles = [child.stdin, child.stdout, child.stderr] as unknown as {
    ref?(): void;
    unref?(): void;
  }[];
  return {
    get pid() {
      return child.pid;
    },
    lines: splitLines(child.stdout),
    exited,
    send(line) {
      if (!stdinOpen || spawnError) throw spawnError ?? new Error("stdin is closed");
      child.stdin.write(`${line}\n`);
    },
    end() {
      if (!stdinOpen) return;
      stdinOpen = false;
      child.stdin.end();
    },
    kill(signal) {
      child.kill(signal);
    },
    ref() {
      child.ref();
      for (const h of handles) h.ref?.();
    },
    unref() {
      child.unref();
      for (const h of handles) h.unref?.();
    },
    stderr: () => err,
  };
}
