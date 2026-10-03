// Deployment logs: the text arrives in chunks (app_deployment_log from a
// byte offset) whenever the event feed says a line was written, so the buffer
// only ever appends what it has not seen, and never shows a line twice.

/** Lines kept in the browser; older ones are dropped (the server keeps the file). */
export const MAX_LINES = 10_000;

export class LogBuffer {
  lines: string[] = [];
  /** A line still being written (no newline yet). */
  partial = "";
  /** Lines dropped off the top to stay under `max`. */
  dropped = 0;
  readonly max: number;

  constructor(max = MAX_LINES) {
    this.max = max;
  }

  /** Append a chunk of text. */
  push(chunk: string): void {
    if (!chunk) return;
    const parts = (this.partial + chunk).split("\n");
    this.partial = parts.pop() ?? "";
    for (const p of parts) this.lines.push(p.replace(/\r$/, ""));
    const over = this.lines.length - this.max;
    if (over > 0) {
      this.lines.splice(0, over);
      this.dropped += over;
    }
  }

  /** Every line, the unfinished one included. */
  all(): string[] {
    return this.partial ? [...this.lines, this.partial] : this.lines.slice();
  }

  text(): string {
    return this.all().join("\n");
  }
}

/**
 * One fetch of a log from `offset`: what to show next. The server answers
 * the text from `offset`, the offset to ask from next, and whether the
 * deployment finished. A reply for an older offset (a slow request racing a
 * newer one) is ignored.
 */
export interface LogChunk {
  log: string;
  offset: number;
  finished: boolean;
}

export class LogFollower {
  readonly buf: LogBuffer;
  offset = 0;
  finished = false;

  constructor(buf = new LogBuffer()) {
    this.buf = buf;
  }

  /** Apply a reply to a request made at `askedAt`; false when it was stale. */
  apply(askedAt: number, r: LogChunk): boolean {
    if (askedAt !== this.offset) return false;
    this.buf.push(r.log);
    this.offset = r.offset;
    if (r.finished) {
      this.finished = true;
      // The last line may lack its newline; it is complete now.
      if (this.buf.partial) {
        this.buf.lines.push(this.buf.partial);
        this.buf.partial = "";
      }
    }
    return true;
  }
}

/** A deployment log line on the event feed: `app NAME: #ID: line`. */
export function deploymentLine(message: string, app: string, id: number): string | null {
  const p = `app ${app}: #${id}: `;
  return message.startsWith(p) ? message.slice(p.length) : null;
}

/** Whether an event's message is about deployment `id` of `app` (a log line or a status change). */
export function concernsDeployment(message: string, app: string, id: number): boolean {
  if (deploymentLine(message, app, id) !== null) return true;
  const p = `app ${app}: deployment ${id}`;
  return message === p || message.startsWith(p + " ") || message.startsWith(p + ":");
}

// ANSI SGR colours, the common subset build tools print.

export interface Span {
  text: string;
  fg?: string;
  bold?: boolean;
  dim?: boolean;
}

const COLORS = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

// eslint-disable-next-line no-control-regex
const CSI = /\x1b\[([0-9;]*)([A-Za-z])/g;

/** Split a line into styled spans; non-colour escape sequences are dropped. */
export function parseAnsi(line: string): Span[] {
  const out: Span[] = [];
  let fg: string | undefined;
  let bold = false;
  let dim = false;
  let last = 0;
  const emit = (text: string) => {
    if (!text) return;
    const prev = out[out.length - 1];
    if (prev && prev.fg === fg && !!prev.bold === bold && !!prev.dim === dim) prev.text += text;
    else out.push({ text, ...(fg ? { fg } : {}), ...(bold ? { bold } : {}), ...(dim ? { dim } : {}) });
  };
  for (const m of line.matchAll(CSI)) {
    emit(line.slice(last, m.index));
    last = m.index! + m[0].length;
    if (m[2] !== "m") continue;
    const codes = m[1] === "" ? [0] : m[1].split(";").map((c) => Number(c) || 0);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0) {
        fg = undefined;
        bold = false;
        dim = false;
      } else if (c === 1) bold = true;
      else if (c === 2) dim = true;
      else if (c === 22) {
        bold = false;
        dim = false;
      } else if (c >= 30 && c <= 37) fg = COLORS[c - 30];
      else if (c >= 90 && c <= 97) fg = "bright-" + COLORS[c - 90];
      else if (c === 39) fg = undefined;
      else if (c === 38 || c === 48) i += codes[i + 1] === 5 ? 2 : codes[i + 1] === 2 ? 4 : 0;
    }
  }
  // eslint-disable-next-line no-control-regex
  emit(line.slice(last).replace(/\x1b[^a-zA-Z]*[a-zA-Z]?/g, ""));
  return out;
}

export function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return s.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "");
}
