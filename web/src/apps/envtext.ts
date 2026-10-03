// The app environment editor's `.env` text, checked and highlighted the way
// the daemon parses it (src/app/env.rs): KEY=value lines, `export` dropped,
// `#` comments, double quotes with escapes (may span lines), single quotes
// literal, ` #` ending an unquoted value, and KEY=${{secret.NAME}} (unquoted)
// referring to an org secret.

export type TokenKind = "comment" | "export" | "key" | "op" | "value" | "quoted" | "secret" | "trailing" | "error";

export interface Token {
  kind: TokenKind;
  text: string;
}

export interface Problem {
  /** 1-based line number. */
  line: number;
  message: string;
  /** Errors stop a save; warnings only explain. */
  severity: "error" | "warning";
}

export interface EnvAnalysis {
  /** One token list per physical line of the text. */
  lines: Token[][];
  problems: Problem[];
  /** KEY -> value or {secret}, in order, last one winning (as the daemon does). */
  vars: Map<string, string | { secret: string }>;
  /** Secret names referred to, sorted, unique. */
  secrets: string[];
}

const SECRET_OPEN = "${{secret.";
const SECRET_CLOSE = "}}";

export function keyProblem(k: string): string | null {
  if (!k) return "a variable needs a name before =";
  if (k.length > 256) return `${k.slice(0, 20)}…: at most 256 characters`;
  if (/^[0-9]/.test(k) || !/^[A-Za-z0-9_]+$/.test(k)) return `${k}: letters, digits and _, not starting with a digit`;
  return null;
}

export function secretNameProblem(n: string): string | null {
  if (!n || n.length > 128 || n.startsWith(".") || !/^[A-Za-z0-9_.-]+$/.test(n)) {
    return `secret name ${JSON.stringify(n)}: 1-128 characters of [A-Za-z0-9_.-], not starting with '.'`;
  }
  return null;
}

/** The index of the first unescaped `"` in `s`, or -1. */
function closingQuote(s: string): number {
  let esc = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (esc) esc = false;
    else if (c === "\\") esc = true;
    else if (c === '"') return i;
  }
  return -1;
}

function unescape(s: string): string {
  return s.replace(/\\(.?)/gs, (_, c: string) =>
    c === "n" ? "\n" : c === "t" ? "\t" : c === "r" ? "\r" : c === "" ? "\\" : c,
  );
}

/** Split `raw` at its leading whitespace, keeping both parts. */
function lead(raw: string): [string, string] {
  const m = /^\s*/.exec(raw)![0];
  return [m, raw.slice(m.length)];
}

/** Parse and tokenize `.env` text. Never throws: problems are reported. */
export function analyzeEnv(text: string): EnvAnalysis {
  const phys = text.split("\n");
  const lines: Token[][] = [];
  const problems: Problem[] = [];
  const vars = new Map<string, string | { secret: string }>();
  const firstSeen = new Map<string, number>();
  const secrets = new Set<string>();
  const err = (line: number, message: string) => problems.push({ line, message, severity: "error" });
  const warn = (line: number, message: string) => problems.push({ line, message, severity: "warning" });

  for (let i = 0; i < phys.length; i++) {
    const n = i + 1;
    const raw = phys[i].replace(/\r$/, "");
    const t = raw.trim();
    if (t === "") {
      lines.push(raw ? [{ kind: "value", text: raw }] : []);
      continue;
    }
    if (t.startsWith("#")) {
      lines.push([{ kind: "comment", text: raw }]);
      continue;
    }
    const toks: Token[] = [];
    let [ws, rest] = lead(raw);
    if (ws) toks.push({ kind: "value", text: ws });
    const ex = /^export\s+/.exec(rest);
    if (ex) {
      toks.push({ kind: "export", text: ex[0] });
      rest = rest.slice(ex[0].length);
    }
    const eq = rest.indexOf("=");
    if (eq < 0) {
      toks.push({ kind: "error", text: rest });
      lines.push(toks);
      err(n, "expected KEY=value");
      continue;
    }
    const keyRaw = rest.slice(0, eq);
    const key = keyRaw.trim();
    const kp = keyProblem(key);
    toks.push({ kind: kp ? "error" : "key", text: keyRaw });
    toks.push({ kind: "op", text: "=" });
    if (kp) err(n, kp);
    let v = rest.slice(eq + 1);
    [ws, v] = lead(v);
    if (ws) toks.push({ kind: "value", text: ws });

    let value: string | { secret: string } | null = null;
    if (v.startsWith('"')) {
      // Double quotes: escapes, and the value may continue on later lines.
      let body = v.slice(1);
      let end = closingQuote(body);
      const spill: string[] = [];
      let j = i;
      while (end < 0 && j + 1 < phys.length) {
        j++;
        spill.push(phys[j].replace(/\r$/, ""));
        body += "\n" + phys[j].replace(/\r$/, "");
        end = closingQuote(body);
      }
      if (end < 0) {
        toks.push({ kind: "error", text: v });
        lines.push(toks);
        for (const s of spill) lines.push([{ kind: "error", text: s }]);
        err(n, "unterminated quote");
        i = j;
        continue;
      }
      const tail = body.slice(end + 1);
      const tailBad = tail.trim() !== "" && !tail.trim().startsWith("#");
      if (tailBad) err(n + spill.length, "text after the closing quote");
      // Tokens: the opening line, then each continuation line.
      const all = ('"' + body.slice(0, end + 1)).split("\n");
      const tailText = tail;
      all.forEach((piece, k) => {
        const target = k === 0 ? toks : [];
        target.push({ kind: "quoted", text: piece });
        if (k === all.length - 1 && tailText) {
          target.push({ kind: tailBad ? "error" : "trailing", text: tailText });
        }
        if (k > 0) lines.push(target);
        else lines.push(toks);
      });
      value = unescape(body.slice(0, end));
      if (value.startsWith(SECRET_OPEN)) {
        warn(n, `${key}: a quoted \${{secret...}} is the literal text, not a secret reference; remove the quotes`);
      }
      i = j;
    } else if (v.startsWith("'")) {
      const end = v.indexOf("'", 1);
      if (end < 0) {
        toks.push({ kind: "error", text: v });
        lines.push(toks);
        err(n, "unterminated quote");
        continue;
      }
      toks.push({ kind: "quoted", text: v.slice(0, end + 1) });
      if (end + 1 < v.length) toks.push({ kind: "trailing", text: v.slice(end + 1) });
      value = v.slice(1, end);
      lines.push(toks);
    } else {
      // Unquoted: ` #` starts a comment, as in docker compose.
      const hash = v.indexOf(" #");
      const body = hash >= 0 ? v.slice(0, hash) : v;
      const trimmed = body.trimEnd();
      const comment = v.slice(trimmed.length);
      if (trimmed.startsWith(SECRET_OPEN) && trimmed.endsWith(SECRET_CLOSE) && trimmed.length >= SECRET_OPEN.length + SECRET_CLOSE.length) {
        const name = trimmed.slice(SECRET_OPEN.length, trimmed.length - SECRET_CLOSE.length);
        const sp = secretNameProblem(name);
        toks.push({ kind: sp ? "error" : "secret", text: trimmed });
        if (sp) err(n, sp);
        else {
          value = { secret: name };
          secrets.add(name);
        }
      } else {
        if (trimmed) toks.push({ kind: "value", text: trimmed });
        if (trimmed.includes(SECRET_OPEN)) {
          warn(n, `${key}: \${{secret.NAME}} is a reference only as the whole value`);
        }
        value = trimmed;
      }
      if (comment) toks.push({ kind: comment.trim() ? "comment" : "value", text: comment });
      lines.push(toks);
    }
    if (!kp && value !== null) {
      const seen = firstSeen.get(key);
      if (seen !== undefined) warn(n, `${key} is also set on line ${seen}; this one wins`);
      else firstSeen.set(key, n);
      vars.set(key, value);
    }
  }
  return { lines, problems, vars, secrets: [...secrets].sort() };
}

/** The secret names `text` refers to that are not in `known`. */
export function missingSecrets(a: EnvAnalysis, known: Iterable<string>): string[] {
  const k = new Set(known);
  return a.secrets.filter((s) => !k.has(s));
}
