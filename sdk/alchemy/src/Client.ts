/**
 * The connection to an isb host: where it is, the org to act in and an API
 * token, plus `call`, which runs one isb tool over its REST API
 * (`POST /orgs/<org>/api/v1/tools/<tool>`).
 */
import * as Context from "effect/Context";
import * as Data from "effect/Data";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";

export interface IsbConfig {
  /** The isb server, e.g. `https://isb.example.com`. */
  readonly url: string;
  /** The org every resource lives in. */
  readonly org: string;
  /** An API token (`isb token create`), a member of `org` or above. */
  readonly token: Redacted.Redacted<string>;
}

/** A tool call isb refused or that never reached it. */
export class IsbError extends Data.TaggedError("IsbError")<{
  /** The tool that was called. */
  readonly tool: string;
  /** isb's error code (`invalid`, `not_found`, `forbidden`, ...), or `unreachable`. */
  readonly code: string;
  /** The HTTP status, 0 when there was no answer. */
  readonly status: number;
  readonly message: string;
  readonly data?: unknown;
}> {}

/**
 * The connection, resolved when a resource first needs it, so a stack that
 * deploys nothing to isb never asks for a token.
 */
export class Credentials extends Context.Service<Credentials, Effect.Effect<IsbConfig, IsbError>>()(
  "IsbCredentials",
) {}

export interface CredentialsInput {
  readonly url: string;
  readonly token: string | Redacted.Redacted<string>;
  /** Default: `default`. */
  readonly org?: string;
}

/** A connection given in code. */
export const fromConfig = (input: CredentialsInput): Layer.Layer<Credentials> =>
  Layer.succeed(
    Credentials,
    Effect.succeed({
      url: input.url,
      org: input.org ?? "default",
      token: typeof input.token === "string" ? Redacted.make(input.token) : input.token,
    }),
  );

/**
 * A connection from the environment the isb CLI reads: `ISB_URL`,
 * `ISB_TOKEN` and, optionally, `ISB_ORG` (default `default`).
 */
export const fromEnv = (): Layer.Layer<Credentials> =>
  Layer.succeed(
    Credentials,
    Effect.suspend(() => {
      const { ISB_URL: url, ISB_TOKEN: token, ISB_ORG: org } = process.env;
      if (!url || !token) {
        return Effect.fail(
          new IsbError({
            tool: "",
            code: "unauthorized",
            status: 0,
            message:
              "isb: set ISB_URL (the server) and ISB_TOKEN (an API token from `isb token create`)",
          }),
        );
      }
      return Effect.succeed({ url, org: org || "default", token: Redacted.make(token) });
    }),
  );

/** Run one isb tool and return its result. */
export const call = <T = unknown>(
  tool: string,
  args: Record<string, unknown> = {},
): Effect.Effect<T, IsbError, Credentials> =>
  Effect.gen(function* () {
    const config = yield* yield* Credentials;
    const base = config.url.replace(/\/+$/, "");
    const url = `${base}/orgs/${encodeURIComponent(config.org)}/api/v1/tools/${tool}`;
    return yield* Effect.tryPromise({
      try: async (signal) => {
        const res = await fetch(url, {
          method: "POST",
          headers: {
            authorization: `Bearer ${Redacted.value(config.token)}`,
            "content-type": "application/json",
          },
          body: JSON.stringify(args),
          signal,
        });
        const text = await res.text();
        let body: { result?: unknown; error?: string; message?: string; data?: unknown };
        try {
          body = JSON.parse(text);
        } catch {
          body = { error: "bad_response", message: text.slice(0, 300) || res.statusText };
        }
        if (!res.ok || body.error !== undefined) {
          throw new IsbError({
            tool,
            code: body.error ?? "error",
            status: res.status,
            message: `isb ${tool}: ${body.message ?? res.statusText}`,
            data: body.data,
          });
        }
        return body.result as T;
      },
      catch: (e) =>
        e instanceof IsbError
          ? e
          : new IsbError({
              tool,
              code: "unreachable",
              status: 0,
              message: `isb ${tool}: ${config.url} did not answer (${String(e)})`,
            }),
    });
  });

/** `call`, with "there is no such thing" as `undefined`. */
export const callFound = <T = unknown>(tool: string, args: Record<string, unknown> = {}) =>
  call<T>(tool, args).pipe(
    Effect.catchTag("IsbError", (e) =>
      e.code === "not_found" ? Effect.succeed(undefined) : Effect.fail(e),
    ),
  );
