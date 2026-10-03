// SSH public keys as people paste them: a quick check before the server's
// own (src/auth/ssh_keys.rs), and the comment as a default name.

const TYPES = [
  "ssh-ed25519",
  "ecdsa-sha2-nistp256",
  "ecdsa-sha2-nistp384",
  "ecdsa-sha2-nistp521",
  "sk-ssh-ed25519@openssh.com",
  "sk-ecdsa-sha2-nistp256@openssh.com",
  "ssh-rsa",
];

/** What is wrong with a pasted public key, or null when it looks right. */
export function publicKeyProblem(text: string): string | null {
  if (text.includes("PRIVATE KEY")) return "That is a private key. Paste the .pub file's contents instead.";
  const lines = text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"));
  if (lines.length === 0) return "Paste a public key.";
  if (lines.length > 1) return "One key at a time.";
  const [type, data] = lines[0].split(/\s+/);
  if (!TYPES.includes(type)) {
    return /[="]/.test(type)
      ? "Paste the key alone, without authorized_keys options."
      : "Not an SSH public key: it starts with ssh-ed25519, ecdsa-sha2-… or ssh-rsa.";
  }
  if (!data || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)) return "The key's data is missing or damaged.";
  return null;
}

/** The comment after the key (often user@host), the server's default name. */
export function keyComment(text: string): string {
  const line = text.trim().split("\n")[0] ?? "";
  return line.split(/\s+/).slice(2).join(" ");
}
