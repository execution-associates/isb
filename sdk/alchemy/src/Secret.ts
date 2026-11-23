import * as Provider from "alchemy/Provider";
import { Resource } from "alchemy/Resource";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";
import { call, callFound } from "./Client.ts";
import { replaceWhen } from "./Replace.ts";

export interface SecretProps {
  /** The secret's name in the org's store. */
  name: string;
  /** Its value. Use `Redacted.make(...)` or `Config.redacted(...)`. */
  value: Redacted.Redacted<string>;
}

export interface SecretAttributes {
  name: string;
  /** The version isb holds now. */
  version: number;
  /** SHA-256 of the value last written, to tell whether it changed. */
  valueHash: string;
  /** What an app's `env` takes to read this secret: `{ secret: name }`. */
  ref: { secret: string };
}

export type Secret = Resource<"Isb.Secret", SecretProps, SecretAttributes>;

const sha256 = (text: string) =>
  Effect.promise(async () => {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
    return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
  });

const base64 = (text: string) => Buffer.from(text, "utf8").toString("base64");

/**
 * A secret in the org's store. isb holds the value; apps and stacks name
 * it, and it reaches them as the one variable or file that asks for it.
 *
 * ```typescript
 * const dbUrl = yield* Isb.Secret("db-url", {
 *   name: "shop-db-url",
 *   value: yield* Config.redacted("DATABASE_URL"),
 * });
 * yield* Isb.App("web", { ..., env: { DATABASE_URL: dbUrl.ref } });
 * ```
 *
 * A new value is a new version in isb, and the services that use the
 * secret roll (or do what their `on_change` says). The same value again
 * writes nothing.
 */
export const Secret = Resource<Secret>("Isb.Secret");

export const SecretProvider = () =>
  Provider.succeed(Secret, {
    stables: ["name", "ref"],
    diff: replaceWhen<SecretProps>((p) => p.name),
    reconcile: Effect.fn(function* ({ news, output }) {
      const valueHash = yield* sha256(Redacted.value(news.value));
      const observed = yield* callFound<{ version: number }>("secret_inspect", {
        name: news.name,
      });
      // The value cannot be read back, so what tells a change is the hash
      // of what this resource last wrote.
      if (observed === undefined || output?.valueHash !== valueHash) {
        yield* call("secret_set", { name: news.name, value: base64(Redacted.value(news.value)) });
      }
      const now = yield* call<{ version: number }>("secret_inspect", { name: news.name });
      return { name: news.name, version: now.version, valueHash, ref: { secret: news.name } };
    }),
    delete: Effect.fn(function* ({ output }) {
      yield* callFound("secret_delete", { name: output.name });
    }),
  });
