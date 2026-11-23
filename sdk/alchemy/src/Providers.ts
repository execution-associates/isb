import * as Provider from "alchemy/Provider";
import * as Layer from "effect/Layer";
import { App, AppProvider } from "./App.ts";
import { type CredentialsInput, fromConfig, fromEnv } from "./Client.ts";
import { Project, ProjectProvider } from "./Project.ts";
import { Secret, SecretProvider } from "./Secret.ts";
import { Stack, StackProvider } from "./Stack.ts";

export class Providers extends Provider.ProviderCollection<Providers>()("Isb") {}

/**
 * The isb resources (`Project`, `Secret`, `App`, `Stack`) and the
 * connection they use: the one given, or `ISB_URL`, `ISB_TOKEN` and
 * `ISB_ORG` from the environment.
 *
 * ```typescript
 * export default Alchemy.Stack(
 *   "shop",
 *   { providers: Isb.providers(), state: Alchemy.localState() },
 *   Effect.gen(function* () { ... }),
 * );
 * ```
 */
export const providers = (connection?: CredentialsInput) =>
  Layer.effect(Providers, Provider.collection([App, Project, Secret, Stack])).pipe(
    Layer.provide(
      Layer.mergeAll(AppProvider(), ProjectProvider(), SecretProvider(), StackProvider()),
    ),
    Layer.provideMerge(connection ? fromConfig(connection) : fromEnv()),
  );
