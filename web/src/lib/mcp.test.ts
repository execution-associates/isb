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
  orgWays,
  memberOffer,
  type OrgAgentIdentities,
  publicMcpUrl,
  subjectKind,
  subjectProblem,
  orgTokenBlocker,
  orgTokenReach,
  rootMcpUrl,
  type SnippetOptions,
  type Viewer,
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

const mapping = (kind: "tailnet" | "access", subject: string) => ({ id: 1, org: "acme", kind, subject, role: "member" as const, note: "", created_at: 0, created_by: "me" });
type Reaches = OrgAgentIdentities["available"]["reach"];
const noReach: Reaches = { platform_admins: { count: 0, who: [] }, access_superadmins: { count: 0, who: [], you: false }, tailnet_superadmins: { count: 0, who: [], you: false } };
const ids = (identities: OrgAgentIdentities["identities"], tailnet_listen: string[], access: boolean, reach: Partial<Reaches> = {}, public_url: string | null = null): OrgAgentIdentities => ({
  identities,
  available: { tailnet_listen, access, public_url, reach: { ...noReach, ...reach } },
});
const state = (d: OrgAgentIdentities | undefined, members: number, viewer?: Viewer) => Object.fromEntries(orgWays(d, members, viewer).map((w) => [w.id, w]));

describe("the org's sign-in cards", () => {
  it("always have the org token on, in the order token, tailnet, access", () => {
    expect(orgWays(undefined, 0).map((w) => w.id)).toEqual(["token", "tailnet", "access"]);
    expect(state(undefined, 0).token.on).toBe(true);
    expect(state(ids([], [], false), 3).token.why).toBeNull();
  });

  it("turn the tailnet on with a tailnet listener and a tailnet mapping, and say what is missing", () => {
    const m = mapping("tailnet", "tag:agents");
    expect(state(ids([m], ["100.86.22.100:8092"], false), 1).tailnet).toMatchObject({ on: true, why: null });
    const noListen = state(ids([m], [], false), 1).tailnet;
    expect(noListen.on).toBe(false);
    expect(noListen.why).toContain("--listen");
    expect(noListen.why).not.toContain("Settings");
    const noMap = state(ids([mapping("access", "svc.access")], ["100.86.22.100:8092"], true), 1).tailnet;
    expect(noMap.on).toBe(false);
    expect(noMap.why).toContain("Settings, Agent identities");
    expect(noMap.why).not.toContain("--listen");
    const neither = state(ids([], [], false), 1).tailnet;
    expect(neither.why).toContain("--listen");
    expect(neither.why).toContain("Settings");
  });

  it("turn Access on with Access on a listener and a mapping or member users", () => {
    expect(state(ids([], [], true), 2).access).toMatchObject({ on: true, why: null });
    expect(state(ids([mapping("access", "svc.access")], [], true), 0).access.on).toBe(true);
    const none = state(ids([], [], true), 0).access;
    expect(none.on).toBe(false);
    expect(none.why).toContain("no member users");
    const noAccess = state(ids([mapping("access", "svc.access")], [], false), 5).access;
    expect(noAccess.on).toBe(false);
    expect(noAccess.why).toContain("CF_ACCESS_TEAM_DOMAIN");
    // A tailnet mapping does not count for Access.
    expect(state(ids([mapping("tailnet", "me@example.com")], [], true), 0).access.on).toBe(false);
  });

  it("turn Access on for people who get in without a mapping: members, platform admins, Access superadmins", () => {
    const viewer = (platformAdmin: boolean, member: boolean) => ({ platformAdmin, member });
    // A member of the org.
    const asMember = state(ids([], [], true), 1, viewer(false, true)).access;
    expect(asMember).toMatchObject({ on: true, why: null });
    expect(asMember.works).toContain("you (a member)");
    // A platform admin, not a member (0 members), reaching every org.
    const asAdmin = state(ids([], [], true, { platform_admins: { count: 1, who: [] } }), 0, viewer(true, false)).access;
    expect(asAdmin.on).toBe(true);
    expect(asAdmin.works).toContain("you (platform admin)");
    // Listed in --superadmin-access, whatever else: on, and says so.
    const reach = { platform_admins: { count: 1, who: [] }, access_superadmins: { count: 1, who: [], you: true } };
    const asSuper = state(ids([], [], true, reach), 0, viewer(true, false)).access;
    expect(asSuper.on).toBe(true);
    expect(asSuper.works).toContain("you (superadmin via Access)");
    expect(asSuper.works).not.toContain("platform admin");
    // Nobody: off, and the reason names every way in.
    const none = state(ids([], [], true), 0, viewer(false, false)).access;
    expect(none.on).toBe(false);
    expect(none.why).toContain("no platform admin");
    expect(none.why).toContain("--superadmin-access");
    // Access not on a listener beats everything.
    expect(state(ids([], [], false, reach), 3, viewer(true, true)).access.on).toBe(false);
  });

  it("name other people only when the server sent their names", () => {
    const hidden = state(ids([], [], true, { platform_admins: { count: 2, who: [] }, access_superadmins: { count: 1, who: [], you: false } }), 0, { platformAdmin: false, member: false }).access.works;
    expect(hidden).toContain("2 other platform admins");
    expect(hidden).toContain("1 other Access superadmin");
    expect(hidden).not.toContain("@");
    const shown = state(ids([], [], true, { access_superadmins: { count: 1, who: ["root@example.com"], you: false } }), 0, { platformAdmin: false, member: false }).access.works;
    expect(shown).toContain("root@example.com");
  });

  it("turn the tailnet on for a --superadmin-tailnet login, which reaches every org", () => {
    const reach = { tailnet_superadmins: { count: 1, who: [], you: true } };
    const t = state(ids([], ["100.86.22.100:8092"], false, reach), 0).tailnet;
    expect(t).toMatchObject({ on: true, why: null });
    expect(t.works).toContain("you (superadmin via the tailnet)");
    expect(t.works).toContain("Any other node needs");
    // Still off without a tailnet listener.
    expect(state(ids([], [], false, reach), 0).tailnet.on).toBe(false);
    expect(state(ids([], ["100.86.22.100:8092"], false), 0).tailnet.on).toBe(false);
  });

  it("show tokenless snippets for the tailnet and service token headers for Access", () => {
    const tail = { name: "isb-acme", url: orgMcpUrl(listenOrigin("100.86.22.100:8092"), "acme"), tokenVar: null, access: false };
    expect(claudeCode(tail).blocks[0].code).toBe("claude mcp add --transport http --scope user isb-acme http://100.86.22.100:8092/orgs/acme/mcp");
    expect(curl(tail).blocks[0].code).not.toContain("Authorization");
    const acc = { name: "isb-acme", url: orgMcpUrl("https://isb.example.com", "acme"), tokenVar: null, access: true };
    const code = claudeCode(acc).blocks[0].code;
    expect(code).toContain('--header "CF-Access-Client-Id: $CF_ACCESS_CLIENT_ID"');
    expect(code).toContain('--header "CF-Access-Client-Secret: $CF_ACCESS_CLIENT_SECRET"');
    expect(code).not.toContain("Authorization");
  });
});

describe("agent identity subjects", () => {
  it("are named by what they are", () => {
    expect(subjectKind(mapping("tailnet", "tag:agents"))).toBe("node tag");
    expect(subjectKind(mapping("tailnet", "me@example.com"))).toBe("login");
    expect(subjectKind(mapping("access", "bob@example.com"))).toBe("email");
    expect(subjectKind(mapping("access", "abc.access"))).toBe("service token");
  });

  it("are checked like the server does", () => {
    expect(subjectProblem("tailnet", "tag:agents")).toBeNull();
    expect(subjectProblem("tailnet", "me@example.com")).toBeNull();
    for (const bad of ["", "nobody", "tag:", "tag:a b", "*@example.com", "a@b,c@d"]) expect(subjectProblem("tailnet", bad), bad).not.toBeNull();
    expect(subjectProblem("access", "abc123.access")).toBeNull();
    expect(subjectProblem("access", "bob@example.com")).toBeNull();
    expect(subjectProblem("access", "@example.com")).not.toBeNull();
    expect(subjectProblem("access", "a*")).not.toBeNull();
  });
});

describe("the Access card's MCP URL", () => {
  it("is the public URL's, never the page's origin, and null without one", () => {
    expect(publicMcpUrl("https://isb-test.execution.associates/", "lab")).toBe("https://isb-test.execution.associates/orgs/lab/mcp");
    expect(publicMcpUrl(null, "lab")).toBeNull();
    expect(publicMcpUrl(undefined, "lab")).toBeNull();
    expect(publicMcpUrl("", "lab")).toBeNull();
  });
});

describe("the inline add-as-member action", () => {
  const isUser = { code: "is_user", status: 409, data: { user_id: 7, email: "knowsuchagency@gmail.com" } };
  it("is offered to owners and admins when the email is an isb user's", () => {
    expect(memberOffer(isUser, "lab", "member", true)).toEqual({ userId: 7, email: "knowsuchagency@gmail.com", label: "Add knowsuchagency@gmail.com to lab as member" });
    expect(memberOffer(isUser, "lab", "viewer", true)?.label).toBe("Add knowsuchagency@gmail.com to lab as viewer");
  });
  it("is not offered to anyone else, or for another error", () => {
    expect(memberOffer(isUser, "lab", "member", false)).toBeNull();
    expect(memberOffer({ code: "conflict", status: 409 }, "lab", "member", true)).toBeNull();
    expect(memberOffer({ code: "is_user", data: { email: "a@b.c" } }, "lab", "member", true)).toBeNull();
    expect(memberOffer(new Error("x"), "lab", "member", true)).toBeNull();
    expect(memberOffer(null, "lab", "member", true)).toBeNull();
  });
});
