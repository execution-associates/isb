// A tab opened before a deploy still runs the old bundle. Its lazy pages
// name chunks by content hash, and the new binary only embeds the new
// hashes, so opening one of those pages 404s on the chunk. The cure is a
// full reload, which fetches the current index.html (served no-store).
//
// One reload per window: if the chunk is still missing right after a
// reload, reloading again would loop forever, so the error is shown instead.

const KEY = "isb-stale-reload";
const WINDOW_MS = 10_000;

/** Whether `err` is a failed fetch of a code-split chunk. */
export function isChunkLoadError(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err ?? "");
  return (
    /Failed to fetch dynamically imported module/i.test(msg) || // Chromium
    /error loading dynamically imported module/i.test(msg) || // Firefox
    /Importing a module script failed/i.test(msg) || // Safari
    /Unable to preload CSS/i.test(msg) // Vite's CSS preload
  );
}

/** Whether a reload is allowed now: none in the last WINDOW_MS. */
export function mayReload(last: string | null, now: number): boolean {
  const t = last === null ? NaN : Number(last);
  return !(t <= now && now - t < WINDOW_MS);
}

/** Reload to pick up the current bundle, unless this window just did.
 *  Returns whether it is reloading. */
export function reloadForNewBundle(): boolean {
  const now = Date.now();
  try {
    if (!mayReload(sessionStorage.getItem(KEY), now)) return false;
    sessionStorage.setItem(KEY, String(now));
  } catch {
    // storage blocked: with no way to remember, a reload could loop
    return false;
  }
  window.location.reload();
  return true;
}
