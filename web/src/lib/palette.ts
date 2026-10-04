// The command palette's matching: forgiving (subsequences match), but a
// word that starts with what you typed beats one that merely contains it.

export interface Command {
  id: string;
  title: string;
  /** Section heading in the list ("Navigate", "Apps", "Actions"...). */
  group: string;
  /** Extra words that should find it (a project name, "dark mode"). */
  keywords?: string[];
  /** Muted text on the right (a path, a status). */
  hint?: string;
  /** A keyboard shortcut to show (e.g. "G P"). */
  shortcut?: string;
}

/** How well `query` (one lowercase token) matches `text`; 0 is no match. */
export function scoreToken(query: string, text: string): number {
  if (!query) return 1;
  const t = text.toLowerCase();
  if (t === query) return 120;
  if (t.startsWith(query)) return 100 - Math.min(20, t.length - query.length) / 2;
  // A word (after a space, slash, dash, dot or underscore) starts with it.
  const words = t.split(/[\s/\-_.·:]+/);
  if (words.some((w) => w.startsWith(query))) return 80;
  if (t.includes(query)) return 60;
  // Subsequence: every character in order, fewer gaps is better.
  let at = -1;
  let gaps = 0;
  for (const ch of query) {
    const next = t.indexOf(ch, at + 1);
    if (next < 0) return 0;
    if (at >= 0 && next > at + 1) gaps++;
    at = next;
  }
  return Math.max(5, 40 - gaps * 6);
}

/** A command's score for a whole query: every token must match its title or a keyword. */
export function scoreCommand(query: string, c: Command): number {
  const tokens = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return 1;
  const fields = [c.title, ...(c.keywords ?? []), c.group];
  let total = 0;
  for (const tok of tokens) {
    let best = 0;
    for (const [i, f] of fields.entries()) {
      // The title counts most, the group least.
      const weight = i === 0 ? 1 : i === fields.length - 1 ? 0.5 : 0.8;
      best = Math.max(best, scoreToken(tok, f) * weight);
    }
    if (best === 0) return 0;
    total += best;
  }
  return total;
}

/**
 * The commands to show for `query`: with no query, in their given order
 * (groups as given); otherwise best first, at most `limit`.
 */
export function filterCommands<C extends Command>(commands: C[], query: string, limit = 50): C[] {
  if (!query.trim()) return commands.slice(0, limit);
  return commands
    .map((c, i) => ({ c, i, s: scoreCommand(query, c) }))
    .filter((x) => x.s > 0)
    .toSorted((a, b) => b.s - a.s || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.c);
}

/** Commands grouped for display, groups in order of first appearance. */
export function groupCommands<C extends Command>(commands: C[]): { group: string; items: C[] }[] {
  const out: { group: string; items: C[] }[] = [];
  for (const c of commands) {
    let g = out.find((x) => x.group === c.group);
    if (!g) {
      g = { group: c.group, items: [] };
      out.push(g);
    }
    g.items.push(c);
  }
  return out;
}

/** Wrap-around movement through `n` items. */
export function move(index: number, delta: number, n: number): number {
  if (n <= 0) return 0;
  return (((index + delta) % n) + n) % n;
}

/**
 * Two-key navigation shortcuts ("g" then a letter), as GitHub and Linear
 * have them: feed each key with the previous pending one; the answer is the
 * target's key, or null with the new pending state.
 */
export function sequence(pending: string | null, key: string, targets: Record<string, string>): { fire: string | null; pending: string | null } {
  const k = key.toLowerCase();
  if (pending === "g") {
    return { fire: targets[k] ?? null, pending: null };
  }
  return { fire: null, pending: k === "g" ? "g" : null };
}
