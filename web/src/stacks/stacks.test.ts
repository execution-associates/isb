import { describe, expect, it } from "vitest";
import type { Project } from "@/apps/api";
import { appStackNames, isComposeStack } from "./api";

const project = (name: string, envs: string[]): Project => ({
  name,
  created_at: 0,
  environments: envs.map((e) => ({ name: e, stack: `${name}-${e}`, apps: [] })),
});

describe("isComposeStack", () => {
  const owned = appStackNames([project("shop", ["production", "staging"]), project("blog", ["production"])]);

  it("is a stack no project environment owns", () => {
    expect(isComposeStack("monitoring", owned)).toBe(true);
    expect(isComposeStack("shop", owned)).toBe(true);
  });

  it("is not the stack of a project's environment, or one of its previews", () => {
    expect(isComposeStack("shop-production", owned)).toBe(false);
    expect(isComposeStack("blog-production", owned)).toBe(false);
    expect(isComposeStack("shop-staging-pr-12", owned)).toBe(false);
    // A near miss is a compose stack.
    expect(isComposeStack("shop-staging-prod", owned)).toBe(true);
  });

  it("is not isb's own tunnel", () => {
    expect(isComposeStack("isb-tunnel", owned)).toBe(false);
  });
});
