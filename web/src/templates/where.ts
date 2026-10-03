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

/** What the project select starts on: the URL's project, else the newest one, else a new one named after the template. */
export function defaultProject(projects: Proj[], templateId: string, wanted?: string | null): { choice: string; newName: string } {
  if (wanted) {
    return projects.some((p) => p.name === wanted) ? { choice: wanted, newName: "" } : { choice: NEW, newName: wanted };
  }
  const newest = projects.toSorted((a, b) => b.created_at - a.created_at)[0];
  return newest ? { choice: newest.name, newName: "" } : { choice: NEW, newName: templateId };
}

/** What the environment select starts on within a project: the wanted one, else production, else the first, else a new "production". */
export function defaultEnvironment(envs: Env[], wanted?: string | null): { choice: string; newName: string } {
  const names = envs.map((e) => e.name);
  if (wanted) return names.includes(wanted) ? { choice: wanted, newName: "" } : { choice: NEW, newName: wanted };
  const pick = names.includes("production") ? "production" : names[0];
  return pick ? { choice: pick, newName: "" } : { choice: NEW, newName: "production" };
}
