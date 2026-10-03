import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { logoSrc } from "./api";
import { TemplateLogo } from "./logo";

const html = (props: { name: string; src?: string }) => renderToStaticMarkup(createElement(TemplateLogo, props));

describe("template logos", () => {
  it("loads isb's copy, never the upstream URL", () => {
    expect(logoSrc({ catalog: "builtin", id: "gitea", logo: "https://raw.example.com/gitea.svg" })).toBe("/api/v1/templates/builtin/gitea/logo");
    expect(logoSrc({ catalog: "dok", id: "a.b_c", logo: "https://x/y.png" })).toBe("/api/v1/templates/dok/a.b_c/logo");
    expect(logoSrc({ catalog: "builtin", id: "whoami" })).toBeUndefined();
  });

  it("shows the logo when there is one", () => {
    const out = html({ name: "Gitea", src: "/api/v1/templates/builtin/gitea/logo" });
    expect(out).toContain('<img src="/api/v1/templates/builtin/gitea/logo" alt=""');
    expect(out).toContain('loading="lazy"');
    expect(out).not.toContain(">G<");
  });

  it("falls back to the initials without one", () => {
    const out = html({ name: "Uptime Kuma" });
    expect(out).not.toContain("<img");
    expect(out).toContain(">UK<");
  });
});
