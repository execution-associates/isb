import * as Provider from "alchemy/Provider";
import { Resource } from "alchemy/Resource";
import * as Effect from "effect/Effect";
import { call, callFound, IsbError } from "./Client.ts";
import { replaceWhen } from "./Replace.ts";

export interface StackProps {
  /** The stack's name: lowercase letters, digits and `-`, at most 30. */
  name: string;
  /** The compose file, as YAML text. */
  compose: string;
  /** Values for `${VAR}` in the compose file. */
  vars?: Record<string, string>;
  /**
   * The project a new stack belongs to. Default: one named like the stack,
   * which isb makes and which is deleted with the stack.
   */
  project?: string;
  /** The project's environment. Default: `production`. */
  environment?: string;
  /**
   * Delete the stack's named volumes when the stack is destroyed.
   * @default false
   */
  removeVolumes?: boolean;
  /**
   * How long the services may take to converge, in seconds.
   * @default 600
   */
  timeoutSeconds?: number;
}

export interface StackAttributes {
  name: string;
  /** Each service's revision: it changes when the service is replaced. */
  revisions: Record<string, string>;
}

export type Stack = Resource<"Isb.Stack", StackProps, StackAttributes>;

interface Status {
  converged: boolean;
  services: { service: string; rev: string; state: string; message?: string }[];
}

/** Poll until every service has converged, or one has given up. */
const settle = (name: string, timeoutSeconds: number) =>
  Effect.gen(function* () {
    const deadline = Date.now() + timeoutSeconds * 1000;
    while (true) {
      const status = yield* call<Status>("stack_status", { name });
      if (status.converged) return status;
      const stuck = status.services.filter((s) => s.state === "paused" || s.state === "failing");
      const late = Date.now() > deadline;
      if (stuck.length > 0 || late) {
        const why = (
          stuck.length > 0 ? stuck : status.services.filter((s) => s.state !== "converged")
        )
          .map((s) => `${s.service}: ${s.state}${s.message ? ` (${s.message})` : ""}`)
          .join("; ");
        return yield* Effect.fail(
          new IsbError({
            tool: "stack_deploy",
            code: late && stuck.length === 0 ? "timeout" : "deploy_failed",
            status: 0,
            message: `isb: stack ${name} did not converge: ${why}`,
          }),
        );
      }
      yield* Effect.sleep("1 second");
    }
  });

/**
 * A compose file run as an isb stack: long-running services with
 * replicas, health checks, a load balancer on published ports and rolling
 * updates.
 *
 * ```typescript
 * yield* Isb.Stack("wiki", {
 *   name: "wiki",
 *   compose: yield* Effect.promise(() => Bun.file("compose.yaml").text()),
 *   vars: { TAG: "1.27" },
 * });
 * ```
 *
 * A deploy replaces only the services whose settings or image digest
 * changed, so deploying the same file again rolls nothing. A stack's name
 * cannot change; changing it replaces the stack.
 */
export const Stack = Resource<Stack>("Isb.Stack");

export const StackProvider = () =>
  Provider.succeed(Stack, {
    stables: ["name"],
    diff: replaceWhen<StackProps>((p) => p.name),
    reconcile: Effect.fn(function* ({ news }) {
      yield* call("stack_deploy", {
        name: news.name,
        compose: news.compose,
        ...(news.vars && { vars: news.vars }),
        ...(news.project && { project: news.project }),
        ...(news.environment && { environment: news.environment }),
      });
      const status = yield* settle(news.name, news.timeoutSeconds ?? 600);
      return {
        name: news.name,
        revisions: Object.fromEntries(status.services.map((s) => [s.service, s.rev])),
      };
    }),
    delete: Effect.fn(function* ({ olds, output }) {
      yield* callFound("stack_remove", {
        name: output.name,
        volumes: olds.removeVolumes ?? false,
      });
      // A stack deployed without `project` got one of its own name from
      // isb. It goes with the stack, unless something else moved in.
      if (olds.project === undefined) {
        yield* call("project_delete", { name: output.name }).pipe(
          Effect.catchTag("IsbError", () => Effect.void),
        );
      }
    }),
  });
