// The logic behind the YAML editors (an app's definition, a compose stack):
// the line diff the review shows, what a server dry run's answer means for
// the Save buttons, and when the text counts as edited.

/** A problem the server placed in the text; `line` is 1-based. */
export interface Problem {
  line?: number;
  column?: number;
  message: string;
}

export type DiffKind = "same" | "add" | "del";

export interface DiffRow {
  kind: DiffKind;
  text: string;
  /** 1-based line numbers in the old and new text. */
  oldNo?: number;
  newNo?: number;
}

/** A row of the review: a line, or a fold of `hidden` unchanged lines. */
export type Shown = DiffRow | { kind: "gap"; hidden: number };

const lines = (s: string): string[] => (s === "" ? [] : s.replace(/\n$/, "").split("\n"));

/**
 * A line diff of two texts (longest common subsequence, after trimming the
 * common head and tail). Past 4M table cells it degrades to "all old lines
 * out, all new lines in" instead of freezing the page.
 */
export function lineDiff(oldText: string, newText: string): DiffRow[] {
  const a = lines(oldText);
  const b = lines(newText);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const ma = a.slice(head, a.length - tail);
  const mb = b.slice(head, b.length - tail);
  const ops: { kind: DiffKind; text: string }[] = a.slice(0, head).map((text) => ({ kind: "same", text }));
  if (ma.length * mb.length > 4_000_000) {
    ops.push(...ma.map((text) => ({ kind: "del" as const, text })), ...mb.map((text) => ({ kind: "add" as const, text })));
  } else {
    const n = ma.length;
    const m = mb.length;
    // t[i][j]: the LCS length of ma[i..] and mb[j..].
    const t = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        t[i][j] = ma[i] === mb[j] ? t[i + 1][j + 1] + 1 : Math.max(t[i + 1][j], t[i][j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (ma[i] === mb[j]) {
        ops.push({ kind: "same", text: ma[i] });
        i++;
        j++;
      } else if (t[i + 1][j] >= t[i][j + 1]) {
        ops.push({ kind: "del", text: ma[i++] });
      } else {
        ops.push({ kind: "add", text: mb[j++] });
      }
    }
    while (i < n) ops.push({ kind: "del", text: ma[i++] });
    while (j < m) ops.push({ kind: "add", text: mb[j++] });
  }
  ops.push(...a.slice(a.length - tail).map((text) => ({ kind: "same" as const, text })));
  let o = 1;
  let nn = 1;
  return ops.map((op) => {
    const row: DiffRow = { ...op };
    if (op.kind !== "add") row.oldNo = o++;
    if (op.kind !== "del") row.newNo = nn++;
    return row;
  });
}

/** How many lines were added and removed. */
export function diffStats(rows: DiffRow[]): { added: number; removed: number } {
  return {
    added: rows.filter((r) => r.kind === "add").length,
    removed: rows.filter((r) => r.kind === "del").length,
  };
}

/** The rows near a change (`context` lines each side), the rest folded. */
export function fold(rows: DiffRow[], context = 3): Shown[] {
  const keep = Array.from({ length: rows.length }, () => false);
  rows.forEach((r, i) => {
    if (r.kind === "same") return;
    for (let k = Math.max(0, i - context); k <= Math.min(rows.length - 1, i + context); k++) keep[k] = true;
  });
  const out: Shown[] = [];
  let hidden = 0;
  rows.forEach((r, i) => {
    if (keep[i]) {
      if (hidden) out.push({ kind: "gap", hidden });
      hidden = 0;
      out.push(r);
    } else {
      hidden++;
    }
  });
  // An all-unchanged text shows nothing, not one big fold.
  if (hidden && out.length) out.push({ kind: "gap", hidden });
  return out;
}

/** Edited, ignoring a final newline the editor or the server adds or drops. */
export function isEdited(text: string, saved: string): boolean {
  return text.replace(/\n+$/, "") !== saved.replace(/\n+$/, "");
}

/** What a server dry run (`app_apply`/`stack_validate`) answered. */
export interface DryRun {
  valid: boolean;
  errors?: Problem[];
  action?: "created" | "updated" | "unchanged";
  changes?: string[] | { service: string; change: string }[];
  name?: string;
  /** stack_validate: a stack by this name is deployed, and who owns it. */
  exists?: boolean;
  managed_by?: string | null;
}

export interface Verdict {
  /** Saving is allowed. */
  ok: boolean;
  problems: Problem[];
  /** Why saving is refused though the text is valid. */
  blocked?: string;
  /** What saving changes, as short labels. */
  changes: string[];
}

/** The Save buttons' view of an app dry run: valid, and an edit of this very app. */
export function appVerdict(r: DryRun, app: string): Verdict {
  const problems = r.errors ?? [];
  const changes = (r.changes ?? []).map((c) => (typeof c === "string" ? c : `${c.service} ${c.change}`));
  if (!r.valid) return { ok: false, problems, changes: [] };
  if (r.action === "created" || (r.name && r.name !== app)) {
    return {
      ok: false,
      problems: [],
      blocked: `This describes a different app (${r.name ?? "?"}), so saving would create it. Keep name: ${app} to edit this one, or create the app from the projects page.`,
      changes,
    };
  }
  return { ok: true, problems: [], changes };
}

/** A stack dry run: the per-service plan, without services that stay as they are. */
export function stackVerdict(r: DryRun, opts: { name: string; creating: boolean }): Verdict {
  const problems = r.errors ?? [];
  const changes = (r.changes ?? [])
    .map((c) => (typeof c === "string" ? c : c.change === "unchanged" ? "" : `${c.service} ${c.change}`))
    .filter(Boolean);
  if (!r.valid) return { ok: false, problems, changes: [] };
  if (r.managed_by === "apps") {
    return { ok: false, problems: [], changes, blocked: `${opts.name} belongs to a project's apps; change it through them, not from a compose file.` };
  }
  if (r.managed_by) {
    return { ok: false, problems: [], changes, blocked: `${opts.name} is managed by isb itself.` };
  }
  if (opts.creating && r.exists) {
    return { ok: false, problems: [], changes, blocked: `A stack named ${opts.name} already exists; open it to edit it.` };
  }
  return { ok: true, problems: [], changes };
}

/** `line N: message`, or just the message. */
export const problemText = (p: Problem): string => (p.line ? `Line ${p.line}: ${p.message}` : p.message);
