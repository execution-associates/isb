// Where a template deploy goes: the project and environment choices the form
// offers, and what each defaults to.

/** The select value that stands for "make a new one". */
export const NEW = "__new__";

interface Env {
  name: string;
}
interface Proj {
  name: string;
  created_at: number;
  environments: Env[];
}

/**
 * What the project select starts on: the URL's project, else always a new
 * project named after the template (numbered if that name is taken), so a
 * template never lands in an existing project by accident.
 */
export function defaultProject(projects: Proj[], template: string, wanted?: string | null): { choice: string; newName: string } {
  if (wanted) {
    return projects.some((p) => p.name === wanted) ? { choice: wanted, newName: "" } : { choice: NEW, newName: wanted };
  }
  const taken = new Set(projects.map((p) => p.name));
  const base = projectSlug(template);
  let name = base;
  for (let n = 2; taken.has(name); n++) name = `${base.slice(0, 24 - `-${n}`.length).replace(/-+$/, "")}-${n}`;
  return { choice: NEW, newName: name };
}

/** What the environment select starts on within a project: the wanted one, else production, else the first, else a new "production". */
export function defaultEnvironment(envs: Env[], wanted?: string | null): { choice: string; newName: string } {
  const names = envs.map((e) => e.name);
  if (wanted) return names.includes(wanted) ? { choice: wanted, newName: "" } : { choice: NEW, newName: wanted };
  const pick = names.includes("production") ? "production" : names[0];
  return pick ? { choice: pick, newName: "" } : { choice: NEW, newName: "production" };
}

/** A template's name as a project name: "Postgres with Adminer" -> "postgres-with-adminer", within the 24-character limit. */
export function projectSlug(name: string): string {
  let s = name.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^[^a-z]+/, "");
  s = s.slice(0, 24).replace(/-+$/, "");
  return s || "app";
}
