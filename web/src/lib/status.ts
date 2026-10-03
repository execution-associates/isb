// One vocabulary for status colour across every page: a status maps to a
// tone, and a tone to its classes. Pages never pick status colours by hand.

export type Tone = "success" | "info" | "warning" | "danger" | "neutral" | "muted";

/** Badge (pill) classes per tone: tinted background, readable text. */
export const TONE_BADGE: Record<Tone, string> = {
  success: "border-success/25 bg-success/10 text-success",
  info: "border-info/25 bg-info/10 text-info",
  warning: "border-warning/30 bg-warning/12 text-[color-mix(in_oklch,var(--warning)_75%,var(--foreground))]",
  danger: "border-destructive/25 bg-destructive/10 text-destructive",
  neutral: "border-border bg-muted text-foreground/80",
  muted: "border-border bg-transparent text-muted-foreground",
};

/** Dot fill per tone. */
export const TONE_DOT: Record<Tone, string> = {
  success: "bg-success",
  info: "bg-info",
  warning: "bg-warning",
  danger: "bg-destructive",
  neutral: "bg-muted-foreground/60",
  muted: "bg-muted-foreground/35",
};

/** Text colour per tone (icons, numbers). */
export const TONE_TEXT: Record<Tone, string> = {
  success: "text-success",
  info: "text-info",
  warning: "text-warning",
  danger: "text-destructive",
  neutral: "text-foreground/80",
  muted: "text-muted-foreground",
};

export type DeploymentStatus = "queued" | "building" | "deploying" | "done" | "failed" | "superseded";

export const DEPLOYMENT_TONE: Record<DeploymentStatus, Tone> = {
  queued: "neutral",
  building: "info",
  deploying: "info",
  done: "success",
  failed: "danger",
  superseded: "muted",
};

export const DEPLOYMENT_LABEL: Record<DeploymentStatus, string> = {
  queued: "Queued",
  building: "Building",
  deploying: "Deploying",
  done: "Done",
  failed: "Failed",
  superseded: "Superseded",
};

/** Statuses still moving: their dot pulses. */
export const inProgress = (s: DeploymentStatus) => s === "queued" || s === "building" || s === "deploying";

/**
 * A deployment's pipeline as steps, for the progress strip: each step is
 * done, current, failed or waiting. A failure is pinned to the step it
 * happened in (building if it never reached deploying).
 */
export type StepState = "done" | "current" | "failed" | "waiting" | "skipped";
export const STEPS = ["queued", "building", "deploying", "done"] as const;
export type Step = (typeof STEPS)[number];

export function stepStates(status: DeploymentStatus, reached: { building: boolean; deploying: boolean }): Record<Step, StepState> {
  const out: Record<Step, StepState> = { queued: "waiting", building: "waiting", deploying: "waiting", done: "waiting" };
  switch (status) {
    case "queued":
      out.queued = "current";
      break;
    case "building":
      out.queued = "done";
      out.building = "current";
      break;
    case "deploying":
      out.queued = out.building = "done";
      out.deploying = "current";
      break;
    case "done":
      out.queued = out.building = out.deploying = out.done = "done";
      break;
    case "superseded":
      out.queued = "failed";
      out.building = out.deploying = out.done = "skipped";
      break;
    case "failed":
      out.queued = "done";
      if (reached.deploying) {
        out.building = "done";
        out.deploying = "failed";
      } else if (reached.building) {
        out.building = "failed";
        out.deploying = "skipped";
      } else {
        out.queued = "failed";
        out.building = out.deploying = "skipped";
      }
      out.done = "skipped";
      break;
  }
  return out;
}
