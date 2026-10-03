// Servers and placement: the words the UI uses for where an org runs, and
// the arguments the forms send (src/daemon/servers.rs, src/servers/vm.rs).
import type { Placement, ServerView, StepState } from "@/api/tools";
import type { Tone } from "@/lib/status";

/** Where an org runs, in a few words, and how isolated that is. */
export function placementLabel(p: Placement | undefined): { where: string; isolation: string } {
  switch (p?.kind) {
    case "vm":
      return { where: "Dedicated VM", isolation: "own kernel" };
    case "server":
      return { where: p.server, isolation: "own host" };
    default:
      return { where: "This host", isolation: "shared kernel" };
  }
}

/** One plain sentence on what keeps the org apart. */
export function isolationText(p: Placement | undefined): string {
  switch (p?.kind) {
    case "vm":
      return "Runs in a VM made for this org alone, with its own kernel and its own incus; nothing else shares it.";
    case "server":
      return `Runs on server ${p.server}, a separate machine; no other host's workloads share its kernel, though other orgs placed on ${p.server} do.`;
    default:
      return "Containers in an incus project with its own bridge, firewall rules and quotas. They share this host's kernel with other orgs.";
  }
}

/** A server's health as a tone and a word. */
export function healthTone(state: ServerView["health"]["state"]): { tone: Tone; label: string } {
  switch (state) {
    case "up":
      return { tone: "success", label: "Up" };
    case "unreachable":
      return { tone: "danger", label: "Unreachable" };
    default:
      return { tone: "muted", label: "Waiting" };
  }
}

/** The first characters of a build hash, enough to tell builds apart. */
export function shortBuild(b: string | null | undefined): string {
  return b ? b.slice(0, 12) : "";
}

/** How a server's isb compares with this control plane's, in words. */
export function versionSkew(v: ServerView["version"]): { tone: Tone; label: string; text: string } | null {
  if (!v || v.isb == null) return null;
  const cp = `isb ${v.control_plane.isb} (build ${shortBuild(v.control_plane.build)})`;
  if (!v.compatible) {
    return {
      tone: "danger",
      label: "Incompatible",
      text: `Speaks agent protocol ${v.protocol ?? "?"}; this control plane speaks ${v.control_plane.protocol}. Calls for its orgs are refused until one of them is upgraded.`,
    };
  }
  if (!v.skew) return { tone: "success", label: "Current", text: `The same build as this control plane, ${cp}.` };
  const ssh = v.ssh ? "" : " SSH to its orgs needs the upgrade.";
  return { tone: "warning", label: "Differs", text: `This control plane runs ${cp}.${ssh}` };
}

export const STEP_TONE: Record<StepState, Tone> = { pending: "muted", running: "info", done: "success", failed: "danger" };

/** Used over total as a percentage (0-100), or null when either is unknown. */
export function percent(used: number | undefined | null, total: number | undefined | null): number | null {
  if (used == null || !total) return null;
  return Math.max(0, Math.min(100, Math.round((used / total) * 100)));
}

/** Seconds between two unix times, as "42s", "3m 5s", "1h 2m". */
export function duration(from: number | null | undefined, to: number | null | undefined): string {
  if (from == null || to == null) return "";
  const s = Math.max(0, Math.round(to - from));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;

function isIpv6(s: string): boolean {
  if (!/^[0-9a-fA-F:.]+$/.test(s) || !s.includes(":")) return false;
  const halves = s.split("::");
  if (halves.length > 2) return false;
  const groups = halves.flatMap((h) => (h ? h.split(":") : []));
  if (groups.some((g) => !/^[0-9a-fA-F]{1,4}$/.test(g))) return false;
  return halves.length === 2 ? groups.length < 8 : groups.length === 8;
}

/** An address or CIDR the box's firewall can take, else why not. */
export function cidrProblem(s: string): string | null {
  const [ip, len, extra] = s.split("/");
  if (extra !== undefined) return `${s}: an address or CIDR`;
  const v4 = IPV4.test(ip);
  if (!v4 && !isIpv6(ip)) return `${s}: an address or CIDR`;
  if (len !== undefined) {
    const n = Number(len);
    if (!/^\d+$/.test(len) || n > (v4 ? 32 : 128)) return `${s}: a prefix length 0-${v4 ? 32 : 128}`;
  }
  return null;
}

/** allow_from from a textarea: one per line or comma-separated. */
export function parseAllowFrom(text: string): { list: string[]; problem: string | null } {
  const list = [...new Set(text.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean))];
  for (const s of list) {
    const p = cidrProblem(s);
    if (p) return { list, problem: p };
  }
  return { list, problem: null };
}

export type BinarySource = "release" | "self" | "version";

export interface AddServerForm {
  name: string;
  ssh: string;
  sshPort: string;
  sshKey: string;
  allowFrom: string;
  address: string;
  agentPort: string;
  publicIngress: boolean;
  source: BinarySource;
  version: string;
}

export const emptyAddServer: AddServerForm = {
  name: "",
  ssh: "",
  sshPort: "22",
  sshKey: "",
  allowFrom: "",
  address: "",
  agentPort: "7443",
  publicIngress: false,
  source: "release",
  version: "",
};

/** Server names: [a-z0-9-], a letter first, at most 40, not "local". */
export function serverNameProblem(n: string): string | null {
  if (!n) return "A name is required.";
  if (n === "local") return "\"local\" means this host.";
  if (n.length > 40 || !/^[a-z][a-z0-9-]*$/.test(n)) return "Lowercase letters, digits and -, starting with a letter, at most 40.";
  return null;
}

function port(s: string, what: string): number {
  const n = Number(s);
  if (!/^\d+$/.test(s.trim()) || n < 1 || n > 65535) throw new Error(`${what} must be a port number (1-65535).`);
  return n;
}

/** server_add's arguments from the wizard (wait: false), or an Error saying what is wrong. */
export function addServerArgs(f: AddServerForm): Record<string, unknown> {
  const name = f.name.trim();
  const np = serverNameProblem(name);
  if (np) throw new Error(np);
  const ssh = f.ssh.trim();
  if (!/^[A-Za-z0-9._-]+@[A-Za-z0-9.:_-]+$/.test(ssh) || ssh.startsWith("-")) throw new Error("SSH target: user@host.");
  if (!f.sshKey.trim()) throw new Error("Paste the SSH private key.");
  const allow = parseAllowFrom(f.allowFrom);
  if (allow.problem) throw new Error(allow.problem);
  const a: Record<string, unknown> = {
    name,
    ssh,
    ssh_port: port(f.sshPort, "SSH port"),
    ssh_key: f.sshKey,
    agent_port: port(f.agentPort, "Agent port"),
    allow_from: allow.list,
    public_ingress: f.publicIngress,
    wait: false,
  };
  if (f.address.trim()) a.address = f.address.trim();
  if (f.source === "self") a.self_binary = true;
  if (f.source === "version") {
    const v = f.version.trim().replace(/^v/, "");
    if (!/^\d+\.\d+\.\d+([-.+][0-9A-Za-z.-]+)?$/.test(v)) throw new Error("Release: a version such as 0.7.0.");
    a.version = v;
  }
  return a;
}

export type PlacementChoice = { kind: "local" } | { kind: "server"; server: string } | { kind: "vm"; cpus: string; memory: string; disk: string };

const SIZE = /^\d+(B|kB|MB|GB|TB|KiB|MiB|GiB|TiB)?$/;

/** The placement (and wait) arguments of org_create for a choice. */
export function placementArgs(c: PlacementChoice): Record<string, unknown> {
  switch (c.kind) {
    case "local":
      return {};
    case "server":
      return { placement: { server: c.server } };
    case "vm": {
      const vm: Record<string, unknown> = {};
      if (c.cpus.trim()) {
        const n = Number(c.cpus);
        if (!Number.isInteger(n) || n < 1 || n > 256) throw new Error("VM CPUs: a whole number from 1 to 256.");
        vm.cpus = n;
      }
      for (const k of ["memory", "disk"] as const) {
        const v = c[k].trim();
        if (!v) continue;
        if (!SIZE.test(v)) throw new Error(`VM ${k}: a size such as ${k === "memory" ? "4GiB" : "40GiB"}.`);
        vm[k] = v;
      }
      return { placement: { vm }, wait: false };
    }
  }
}
