import { describe, expect, it } from "vitest";
import { keyComment, publicKeyProblem } from "./ssh";

const ED = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh alice@laptop";

describe("pasted SSH keys", () => {
  it("accepts a public key, with or without a comment", () => {
    expect(publicKeyProblem(ED)).toBeNull();
    expect(publicKeyProblem(`  ${ED}\n`)).toBeNull();
    expect(publicKeyProblem(ED.split(" ").slice(0, 2).join(" "))).toBeNull();
    expect(keyComment(ED)).toBe("alice@laptop");
    expect(keyComment("ssh-ed25519 AAAA")).toBe("");
  });

  it("says what is wrong otherwise", () => {
    expect(publicKeyProblem("")).toMatch(/Paste/);
    expect(publicKeyProblem(`${ED}\n${ED}`)).toMatch(/One key/);
    expect(publicKeyProblem("-----BEGIN OPENSSH PRIVATE KEY-----")).toMatch(/private key/);
    expect(publicKeyProblem(`command="sh" ${ED}`)).toMatch(/options/);
    expect(publicKeyProblem("ssh-dss AAAA")).toMatch(/Not an SSH public key/);
    expect(publicKeyProblem("ssh-ed25519 !!!")).toMatch(/damaged/);
  });
});
