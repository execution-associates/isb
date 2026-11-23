import * as Provider from "alchemy/Provider";
import { Resource } from "alchemy/Resource";
import * as Effect from "effect/Effect";
import { call, callFound } from "./Client.ts";
import { replaceWhen } from "./Replace.ts";

export interface ProjectProps {
  /** The project's name in isb: lowercase letters, digits and `-`. */
  name: string;
  description?: string;
  /**
   * The environments to make sure exist. Each one runs as its own stack,
   * `<project>-<environment>`.
   * @default ["production"]
   */
  environments?: string[];
}

export interface ProjectAttributes {
  name: string;
  environments: string[];
}

export type Project = Resource<"Isb.Project", ProjectProps, ProjectAttributes>;

interface ProjectRow {
  name: string;
  environments: (string | { name: string })[];
}

const environmentsOf = (p: ProjectRow): string[] =>
  p.environments.map((e) => (typeof e === "string" ? e : e.name));

const find = (name: string) =>
  call<{ projects: ProjectRow[] }>("project_list").pipe(
    Effect.map((r) => r.projects.find((p) => p.name === name)),
  );

/**
 * An isb project: the group apps and stacks belong to, with one or more
 * environments.
 *
 * ```typescript
 * const shop = yield* Isb.Project("shop", {
 *   name: "shop",
 *   environments: ["production", "staging"],
 * });
 * ```
 *
 * Removing an environment from `environments` leaves it in isb. Destroying
 * the project deletes it only once it holds no apps and no stacks.
 */
export const Project = Resource<Project>("Isb.Project");

export const ProjectProvider = () =>
  Provider.succeed(Project, {
    stables: ["name"],
    // A project cannot be renamed.
    diff: replaceWhen<ProjectProps>((p) => p.name),
    reconcile: Effect.fn(function* ({ news }) {
      const wanted = news.environments ?? ["production"];
      let project = yield* find(news.name);
      if (project === undefined) {
        yield* call("project_create", {
          name: news.name,
          description: news.description ?? "",
          environments: wanted,
        });
        project = yield* find(news.name);
      }
      const have = project ? environmentsOf(project) : [];
      for (const env of wanted.filter((e) => !have.includes(e))) {
        yield* call("environment_create", { project: news.name, name: env });
        have.push(env);
      }
      return { name: news.name, environments: have };
    }),
    delete: Effect.fn(function* ({ output }) {
      yield* callFound("project_delete", { name: output.name });
    }),
    read: Effect.fn(function* ({ olds, output }) {
      const name = output?.name ?? olds?.name;
      if (name === undefined) return undefined;
      const project = yield* find(name);
      return project && { name, environments: environmentsOf(project) };
    }),
  });
