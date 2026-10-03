// The footer of a settings card: what saving does, Discard, and a Save
// button that is live only with changes and says "Saved" for a moment after.
import { Check, Loader2 } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";

export function SaveFooter({
  dirty,
  pending,
  saved,
  onDiscard,
  label = "Save",
  note,
  invalid,
}: {
  dirty: boolean;
  pending: boolean;
  /** True for a few seconds after a save. */
  saved: boolean;
  onDiscard: () => void;
  label?: string;
  /** What saving does, left of the buttons (hidden on phones while saved). */
  note?: ReactNode;
  invalid?: boolean;
}) {
  const showSaved = saved && !dirty && !pending;
  return (
    <>
      <span className="mr-auto min-w-0 text-xs text-muted-foreground">
        {showSaved ? (
          <span className="inline-flex animate-fade-up items-center gap-1.5 font-medium text-success">
            <Check className="size-3.5" />
            Saved
          </span>
        ) : dirty ? (
          <span className="inline-flex items-center gap-1.5">
            <span className="size-1.5 rounded-full bg-warning" aria-hidden />
            Unsaved changes
          </span>
        ) : (
          <span className="hidden sm:inline">{note}</span>
        )}
      </span>
      {dirty && (
        <Button type="button" variant="ghost" onClick={onDiscard} disabled={pending}>
          Discard
        </Button>
      )}
      <Button type="submit" disabled={!dirty || pending || invalid}>
        {pending && <Loader2 className="animate-spin" />}
        {label}
      </Button>
    </>
  );
}
