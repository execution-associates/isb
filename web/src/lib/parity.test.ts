// The parity check (docs/reference/parity.md): every tool is on the parity
// page, every API call the web UI makes is a tool or an endpoint the OpenAPI
// document knows, every identity endpoint has a tool or a documented reason
// for having none, and the page's summary counts are the real ones.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const web = fileURLToPath(new URL("../..", import.meta.url));
const doc = readFileSync(join(web, "../docs/reference/parity.md"), "utf8");

interface Op {
  operationId: string;
  tags?: string[];
  "x-isb-tool"?: string;
  "x-isb-browser-only"?: string;
}
const spec = JSON.parse(readFileSync(join(web, "openapi.json"), "utf8")) as { paths: Record<string, Record<string, Op>> };
const ops = Object.entries(spec.paths).flatMap(([path, item]) => Object.entries(item).map(([method, op]) => ({ path, method, op })));
const tools = new Set(ops.filter((o) => o.path.startsWith("/api/v1/tools/")).map((o) => o.op.operationId));
const identity = ops.filter((o) => o.path.startsWith("/api/v1/auth/"));
const surface = ops.filter((o) => o.op.tags?.includes("surface"));

/** Every source file of the UI, but generated code and tests. */
function sources(dir = join(web, "src")): { file: string; text: string }[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return sources(p);
    if (!/\.tsx?$/.test(name) || name.includes(".test.") || name === "openapi.gen.ts") return [];
    return [{ file: p.slice(web.length), text: readFileSync(p, "utf8") }];
  });
}
const src = sources();

/** Names in backticks on the parity page. */
const documented = new Set([...doc.matchAll(/`([a-z][a-z0-9_]*)`/g)].map((m) => m[1]));

/** Tools the page marks MCP/CLI only: the backticked names in the MCP cell of those rows. */
const mcpOnly = new Set(
  doc
    .split("\n")
    .filter((l) => l.startsWith("|") && l.includes("*MCP/CLI only*"))
    .flatMap((l) => {
      const cells = l.split("|");
      return [...cells[cells.length - 2].matchAll(/`([a-z_]+)`/g)].map((m) => m[1]);
    }),
);

describe("API parity", () => {
  it("lists every tool on the parity page", () => {
    const missing = [...tools].filter((t) => !documented.has(t));
    expect(missing, "add these tools to docs/reference/parity.md").toEqual([]);
  });

  it("only names tools that exist", () => {
    for (const t of mcpOnly) expect(tools.has(t), `${t} on the parity page is not a tool`).toBe(true);
  });

  it("calls only tools that exist", () => {
    const called = new Set(src.flatMap(({ text }) => [...text.matchAll(/\b(?:callTool|wsCall)(?:<[^>]*>)?\(\s*"([a-z_]+)"/g)].map((m) => m[1])));
    expect(called.size).toBeGreaterThan(50);
    const unknown = [...called].filter((t) => !tools.has(t));
    expect(unknown, "the web UI calls tools the daemon does not offer").toEqual([]);
    // A tool marked MCP/CLI only is not called by the UI.
    expect([...called].filter((t) => mcpOnly.has(t)), "called by the UI, yet marked MCP/CLI only").toEqual([]);
  });

  it("calls only documented endpoints, each with a tool or a reason", () => {
    const auth = src.find((s) => s.file.endsWith("api/auth.ts"))!.text;
    const used = [...auth.matchAll(/call\("(get|post|put|patch|delete)", "(\/api\/v1\/auth\/[^"]+)"/g)].map((m) => ({ method: m[1], path: m[2] }));
    expect(used.length).toBeGreaterThan(30);
    for (const u of used) {
      const op = spec.paths[u.path]?.[u.method];
      expect(op, `${u.method} ${u.path}`).toBeDefined();
      expect(!!op!["x-isb-tool"] || !!op!["x-isb-browser-only"], `${u.method} ${u.path}: no tool and no reason`).toBe(true);
    }
    // Streams, websockets, logos and the tool list the UI reaches outside the tools.
    const literal = new Set(src.flatMap(({ text }) => [...text.matchAll(/(\/api\/v1\/(?:events|audit\/stream|history\/stream|terminal|ssh|tools|templates)\b)/g)].map((m) => m[1])));
    for (const l of literal) {
      expect(Object.keys(spec.paths).some((p) => p.includes(l)), `${l} is not in the OpenAPI document`).toBe(true);
    }
  });

  it("gives every identity endpoint a tool or a documented reason", () => {
    expect(identity.length).toBeGreaterThan(30);
    const bad = identity
      .filter(({ op }) => {
        const tool = op["x-isb-tool"];
        return tool ? !tools.has(tool) : !op["x-isb-browser-only"];
      })
      .map(({ path, method }) => `${method} ${path}`);
    expect(bad, "identity endpoints with neither a tool that exists nor a reason").toEqual([]);
  });

  it("has the real counts in its summary", () => {
    const account = new Set(identity.map((o) => o.op["x-isb-tool"]).filter((t): t is string => !!t));
    const count = (label: string) => {
      const row = doc.split("\n").find((l) => l.startsWith(`| ${label} |`));
      expect(row, `summary row "${label}"`).toBeDefined();
      return Number(row!.split("|")[2].trim());
    };
    expect(count("Tools in the web UI and MCP")).toBe(tools.size - account.size - mcpOnly.size);
    expect(count("Account tools, the web UI through the identity endpoints")).toBe(account.size);
    expect(count("Tools for MCP and the CLI only")).toBe(mcpOnly.size);
    expect(count("Identity endpoints with a tool")).toBe(identity.filter((o) => o.op["x-isb-tool"]).length);
    expect(count("Identity endpoints for the browser only")).toBe(identity.filter((o) => o.op["x-isb-browser-only"]).length);
    expect(count("Other routes with no tool")).toBe(surface.filter((o) => o.op["x-isb-browser-only"]).length);
  });
});
