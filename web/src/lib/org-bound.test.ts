// An org-bound endpoint (/orgs/<org>/api/v1/tools/...) scopes a superadmin or
// platform admin down to an admin of that one org, so the host, superadmin
// and platform tools are not on it. The UI calls them on the unbound path.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const web = fileURLToPath(new URL("../..", import.meta.url));

// crates/isb-daemon/src/daemon/authorize.rs PLATFORM_TOOLS and superadmin.rs TOOLS.
const UNBOUND = [
  "server_status", "org_list", "org_create", "org_update", "org_delete", "registry_gc", "notification_settings",
  "template_catalog_add", "template_catalog_remove", "audit_verify", "server_add", "server_list", "server_show",
  "server_remove", "server_rotate_cert", "server_provision_get", "server_upgrade", "user_list", "user_update",
  "host_inventory", "host_policy", "superadmin_token_list", "superadmin_token_revoke", "org_nesting",
];

function sources(dir = join(web, "src")): { file: string; text: string }[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return sources(p);
    if (!/\.tsx?$/.test(name) || name.includes(".test.") || name === "openapi.gen.ts") return [];
    return [{ file: p.slice(web.length), text: readFileSync(p, "utf8") }];
  });
}

describe("org-bound paths", () => {
  it("never carry a host, superadmin or platform tool", () => {
    const bad: string[] = [];
    for (const { file, text } of sources()) {
      for (const [i, line] of text.split("\n").entries()) {
        // callTool("tool", args, org): a third argument binds the call to an org.
        const m = line.match(/callTool(?:<[^(]*>)?\(\s*"([a-z_]+)"(.*)\)/);
        if (!m || !UNBOUND.includes(m[1])) continue;
        if (/,\s*(org|o|orgName)\s*$/.test(m[2])) bad.push(`${file}:${i + 1} ${m[1]}`);
      }
    }
    expect(bad).toEqual([]);
  });
});
