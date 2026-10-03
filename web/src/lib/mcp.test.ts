import { describe, expect, it } from "vitest";
import type { Me, Membership } from "@/api/auth";
import { crumbsFor } from "@/lib/crumbs";
import {
  claudeCode,
  codex,
  curl,
  cursor,
  exportLine,
  firstSentence,
  isDirectHost,
  isHostTool,
  isTailnetListen,
  listenOrigin,
  orgMcpUrl,
  orgTokenBlocker,
  orgTokenReach,
  rootMcpUrl,
  type SnippetOptions,
} from "@/lib/mcp";

const base: SnippetOptions = { name: "isb-acme", url: "https://isb.example.com/orgs/acme/mcp", tokenVar: "ISB_TOKEN", access: false };

function me(memberships: Membership[], extra: Partial<Me> = {}): Me {
  return {
    user: { id: 1, email: "a@x.io", name: "", platform_admin: false, created_at: 0, disabled: false, has_password: true },
    platform_admin: false,
    memberships,
    orgs: memberships.map((m) => m.org),
    auth: { kind: "session", id: 1 },
    superadmin: null,
    ...extra,
  };
}

describe("endpoints", () => {
  it("are built from the origin", () => {
    expect(orgMcpUrl("https://isb.example.com/", "acme")).toBe("https://isb.example.com/orgs/acme/mcp");
    expect(rootMcpUrl("http://127.0.0.1:8092")).toBe("http://127.0.0.1:8092/mcp");
    expect(listenOrigin("100.86.22.100:8092")).toBe("http://100.86.22.100:8092");
    expect(listenOrigin("fd7a:115c:a1e0::1:8092")).toBe("http://[fd7a:115c:a1e0::1]:8092");
  });

  it("know local and tailnet addresses from tunnelled ones", () => {
    for (const h of ["localhost", "127.0.0.1", "[::1]", "100.86.22.100", "titan.tail1234.ts.net", "fd7a:115c:a1e0::5"]) expect(isDirectHost(h), h).toBe(true);
    for (const h of ["isb.example.com", "100.128.0.1", "10.0.0.1", "192.168.1.2"]) expect(isDirectHost(h), h).toBe(false);
    expect(isTailnetListen("100.86.22.100:8092")).toBe(true);
    expect(isTailnetListen("127.0.0.1:8092")).toBe(false);
  });

  it("have a page path the server's MCP route does not shadow", () => {
    expect(crumbsFor("/orgs/acme/agents")).toEqual([{ label: "MCP", to: "/orgs/acme/agents" }]);
    expect(crumbsFor("/agents")).toEqual([{ label: "MCP" }]);
  });
});

describe("org tokens", () => {
  it("are offered to members and platform admins", () => {
    expect(orgTokenBlocker(me([{ org: "acme", role: "member" }]), "acme")).toBeNull();
    expect(orgTokenBlocker(me([{ org: "acme", role: "viewer" }]), "acme")).toBeNull();
    expect(orgTokenBlocker(me([], { platform_admin: true, orgs: ["acme"] }), "acme")).toBeNull();
    expect(orgTokenBlocker(me([{ org: "other", role: "owner" }]), "acme")).toMatch(/aren't a member of acme/);
  });

  it("are not offered to an accountless superadmin or a narrowed token", () => {
    const sa = me([], { superadmin: { source: "token:ci", via: { kind: "token", id: 1, name: "ci" }, account: false } });
    expect(orgTokenBlocker(sa, "acme")).toMatch(/no isb account/);
    const scoped = me([{ org: "acme", role: "owner" }], { auth: { kind: "api_token", id: 2, org: "acme", name: "t", scopes: ["deploy"] } });
    expect(orgTokenBlocker(scoped, "acme")).toMatch(/narrowed/);
    const other = me([{ org: "acme", role: "owner" }, { org: "b", role: "owner" }], { auth: { kind: "api_token", id: 2, org: "b", name: "t" } });
    expect(orgTokenBlocker(other, "acme")).toMatch(/token for b/);
  });

  it("say what they reach", () => {
    expect(orgTokenReach(me([{ org: "acme", role: "viewer" }]), "acme")).toMatch(/only reads/);
    expect(orgTokenReach(me([{ org: "acme", role: "member" }]), "acme")).toMatch(/nothing outside acme/);
    expect(orgTokenReach(me([], { platform_admin: true }), "acme")).toMatch(/\(owner\)/);
  });
});

describe("snippets", () => {
  it("Claude Code: the CLI expands the variable, .mcp.json keeps a reference", () => {
    const s = claudeCode(base);
    expect(s.blocks[0].code).toBe(
      'claude mcp add --transport http --scope user isb-acme https://isb.example.com/orgs/acme/mcp \\\n  --header "Authorization: Bearer $ISB_TOKEN"',
    );
    expect(JSON.parse(s.blocks[1].code)).toEqual({
      mcpServers: { "isb-acme": { type: "http", url: base.url, headers: { Authorization: "Bearer ${ISB_TOKEN}" } } },
    });
    expect(s.note).toMatch(/never should be/);
  });

  it("Codex: the CLI flag, or config.toml naming the variable", () => {
    const s = codex(base);
    expect(s.blocks[0].code).toBe("codex mcp add isb-acme --url https://isb.example.com/orgs/acme/mcp \\\n  --bearer-token-env-var ISB_TOKEN");
    expect(s.blocks[1].code).toBe('[mcp_servers.isb-acme]\nurl = "https://isb.example.com/orgs/acme/mcp"\nbearer_token_env_var = "ISB_TOKEN"');
  });

  it("Cursor: ${env:VAR} in headers", () => {
    expect(JSON.parse(cursor(base).blocks[0].code)).toEqual({ mcpServers: { "isb-acme": { url: base.url, headers: { Authorization: "Bearer ${env:ISB_TOKEN}" } } } });
  });

  it("curl: a tools/list call", () => {
    expect(curl(base).blocks[0].code).toBe(
      [
        "curl -sS https://isb.example.com/orgs/acme/mcp \\",
        '  -H "Authorization: Bearer $ISB_TOKEN" \\',
        '  -H "Content-Type: application/json" \\',
        `  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'`,
      ].join("\n"),
    );
  });

  it("add Access service token headers when asked", () => {
    const o = { ...base, access: true };
    expect(claudeCode(o).blocks[0].code).toContain('--header "CF-Access-Client-Id: $CF_ACCESS_CLIENT_ID"');
    expect(JSON.parse(cursor(o).blocks[0].code).mcpServers["isb-acme"].headers["CF-Access-Client-Secret"]).toBe("${env:CF_ACCESS_CLIENT_SECRET}");
    // codex mcp add can't set headers: the config file only.
    const c = codex(o);
    expect(c.blocks).toHaveLength(1);
    expect(c.blocks[0].code).toContain('env_http_headers = { "CF-Access-Client-Id" = "CF_ACCESS_CLIENT_ID", "CF-Access-Client-Secret" = "CF_ACCESS_CLIENT_SECRET" }');
    expect(curl(o).blocks[0].code).toContain('-H "CF-Access-Client-Secret: $CF_ACCESS_CLIENT_SECRET"');
  });

  it("send no Authorization without a token (a tailnet superadmin)", () => {
    const o = { ...base, name: "isb", url: "http://100.86.22.100:8092/mcp", tokenVar: null };
    expect(claudeCode(o).blocks[0].code).toBe("claude mcp add --transport http --scope user isb http://100.86.22.100:8092/mcp");
    expect(JSON.parse(cursor(o).blocks[0].code).mcpServers.isb.headers).toBeUndefined();
    expect(codex(o).blocks[1].code).not.toContain("bearer_token_env_var");
    expect(curl(o).blocks[0].code).not.toContain("Authorization");
  });

  it("quote the token in the export line", () => {
    expect(exportLine("ISB_TOKEN", "isb_tok_abc")).toBe("export ISB_TOKEN='isb_tok_abc'");
    expect(exportLine("X", "a'b")).toBe(`export X='a'\\''b'`);
  });
});

describe("tool list", () => {
  it("keeps host tools for superadmins and one line per tool", () => {
    expect(isHostTool("host_policy")).toBe(true);
    expect(isHostTool("superadmin_token_list")).toBe(true);
    expect(isHostTool("stack_list")).toBe(false);
    expect(firstSentence("Deploy a stack. Returns changes.")).toBe("Deploy a stack.");
    expect(firstSentence("No full stop")).toBe("No full stop");
    expect(firstSentence(undefined)).toBe("");
  });
});
