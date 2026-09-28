import { accessSync, constants, statSync } from "node:fs";
import { createRequire } from "node:module";
import { delimiter, dirname, join } from "node:path";

/** npm package holding the static isb binary for this platform, if any. */
export function platformPackage(
  platform: string = process.platform,
  arch: string = process.arch,
): string | undefined {
  if (platform !== "linux") return undefined;
  if (arch === "x64") return "@execution-associates/isb-linux-x64";
  if (arch === "arm64") return "@execution-associates/isb-linux-arm64";
  return undefined;
}

function isExecutable(p: string): boolean {
  try {
    if (!statSync(p).isFile()) return false;
    accessSync(p, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function fromPlatformPackage(): string | undefined {
  const pkg = platformPackage();
  if (!pkg) return undefined;
  try {
    const require = createRequire(import.meta.url);
    const bin = join(dirname(require.resolve(`${pkg}/package.json`)), "bin", "isb");
    return isExecutable(bin) ? bin : undefined;
  } catch {
    return undefined;
  }
}

function fromPath(name = "isb"): string | undefined {
  for (const dir of (process.env.PATH ?? "").split(delimiter)) {
    if (!dir) continue;
    const p = join(dir, name);
    if (isExecutable(p)) return p;
  }
  return undefined;
}

/**
 * Find the isb binary: `explicit`, else `$ISB_BIN`, else the platform package
 * (`@execution-associates/isb-linux-x64` / `-linux-arm64`), else `isb` on PATH.
 */
export function findIsb(explicit?: string): string {
  if (explicit) return explicit;
  const env = process.env.ISB_BIN;
  if (env) return env;
  const found = fromPlatformPackage() ?? fromPath();
  if (found) return found;
  const pkg = platformPackage();
  throw new Error(
    "isb binary not found: pass isbBin, set ISB_BIN, " +
      (pkg ? `install ${pkg}, ` : "") +
      "or put isb (0.1.1 or later, with `isb rpc`) on PATH",
  );
}
