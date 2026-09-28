/** Standard base64 with padding, as the protocol uses for binary data. */
export function b64encode(data: string | Uint8Array): string {
  const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
  return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString("base64");
}

export function b64decode(s: string): Uint8Array {
  const b = Buffer.from(s, "base64");
  return new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
}

/** Durations: a number is seconds; a string is passed as is (`"90s"`, `"5m"`). */
export type Duration = number | string;

export function durationParam(d: Duration | undefined | null): string | undefined {
  if (d === undefined || d === null) return undefined;
  if (typeof d === "number") {
    if (!Number.isFinite(d) || d < 0) throw new RangeError(`invalid duration ${d}`);
    return `${d}`;
  }
  return d;
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Drop keys whose value is undefined, so they are omitted from the JSON. */
export function compact<T extends Record<string, unknown>>(o: T): Partial<T> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(o)) if (v !== undefined) out[k] = v;
  return out as Partial<T>;
}

const decoder = new TextDecoder();
export function utf8(b: Uint8Array): string {
  return decoder.decode(b);
}
