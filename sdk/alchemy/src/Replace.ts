import { isResolved, type ReplaceDiff } from "alchemy/Diff";
import type { Input } from "alchemy/Input";
import * as Effect from "effect/Effect";

/**
 * A provider `diff` for resources whose identity isb cannot change: when
 * what `identity` returns differs between the old and new props, the
 * resource is replaced. Anything else is left to alchemy's own comparison
 * of the props.
 */
export const replaceWhen =
  <P>(identity: (props: P) => unknown, options: { deleteFirst?: boolean } = {}) =>
  ({ news, olds }: { news: Input<P>; olds: P }): Effect.Effect<ReplaceDiff | undefined> =>
    Effect.sync(() => {
      if (!isResolved(news) || olds === undefined) return undefined;
      const same = JSON.stringify(identity(news)) === JSON.stringify(identity(olds));
      return same ? undefined : { action: "replace", deleteFirst: options.deleteFirst };
    });
