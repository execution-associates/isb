/** "3 minutes ago", "in 2 days", from unix seconds. */
export function relativeTime(unixSeconds: number | null | undefined, now = Date.now()): string {
  if (!unixSeconds) return "never";
  const diff = Math.round(unixSeconds - now / 1000);
  const abs = Math.abs(diff);
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31_536_000],
    ["month", 2_592_000],
    ["day", 86_400],
    ["hour", 3600],
    ["minute", 60],
  ];
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  for (const [u, s] of units) {
    if (abs >= s) return rtf.format(Math.round(diff / s), u);
  }
  return abs < 10 ? "just now" : rtf.format(diff, "second");
}

export function dateTime(unixSeconds: number | null | undefined): string {
  if (!unixSeconds) return "";
  return new Date(unixSeconds * 1000).toLocaleString();
}

export function initials(name: string, email: string): string {
  const src = name.trim() || email.split("@")[0];
  const parts = src.split(/[\s._-]+/).filter(Boolean);
  const s = parts.length > 1 ? parts[0][0] + parts[1][0] : src.slice(0, 2);
  return s.toUpperCase();
}

/** A browser and OS from a user agent string, roughly. */
export function describeAgent(ua: string | null | undefined): string {
  if (!ua) return "Unknown device";
  const browser =
    /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : /curl\//.test(ua) ? "curl" : "Browser";
  const os = /Windows/.test(ua)
    ? "Windows"
    : /iPhone|iPad/.test(ua)
      ? "iOS"
      : /Mac OS X/.test(ua)
        ? "macOS"
        : /Android/.test(ua)
          ? "Android"
          : /Linux/.test(ua)
            ? "Linux"
            : "";
  return os ? `${browser} on ${os}` : browser;
}
