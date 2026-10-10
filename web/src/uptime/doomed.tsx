// The monitors a delete takes along, named in its confirmation, with the
// choice to keep them.

/** Nothing when no monitor follows what is deleted. */
export function DoomedMonitors({ names, keep, onKeep }: { names: string[]; keep: boolean; onKeep: (v: boolean) => void }) {
  if (!names.length) return null;
  const n = names.length === 1 ? "the monitor" : `the ${names.length} monitors`;
  return (
    <label className="flex items-start gap-2.5 text-[13px]">
      <input type="checkbox" className="mt-0.5 size-4 accent-destructive" checked={!keep} onChange={(e) => onKeep(!e.target.checked)} />
      <span className="min-w-0">
        Also delete {n} watching it
        <span className="block text-xs text-muted-foreground">
          {keep ? "Kept, they fail until you remove them: " : "With their history: "}
          <span className="font-mono">{names.join(", ")}</span>.
        </span>
      </span>
    </label>
  );
}
