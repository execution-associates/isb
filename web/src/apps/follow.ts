// Following one deployment live: its log text (by byte offset, so nothing
// shows twice) and its record (status, image, timings), both from
// app_deployment_log, which answers them together. The page pulls whenever
// the event feed says this deployment wrote a line or changed state; this
// class only decides what a reply changes, so it is tested without a server.
import type { Deployment, DeploymentStatus } from "./api";
import { concernsDeployment, deploymentLine, type LogChunk, LogFollower } from "./logstream";

export interface LogReply extends LogChunk {
  status?: DeploymentStatus;
  deployment?: Deployment;
}

const RANK: Record<DeploymentStatus, number> = { queued: 0, building: 1, deploying: 2, done: 3, failed: 3, superseded: 3 };

export class DeploymentFollow {
  readonly log = new LogFollower();
  record: Deployment | null;
  /** The pipeline stages this deployment was seen in (for the progress strip after a failure). */
  reached = { building: false, deploying: false };
  /** When the first line arrived (ms), for "time to first log line". */
  firstLineAt: number | null = null;
  /** The newest line, from a reply or straight off the event feed. */
  lastLine = "";

  constructor(seed?: Deployment | null) {
    this.record = seed ?? null;
    if (seed) this.see(seed);
  }

  get finished(): boolean {
    return this.log.finished;
  }

  /** Apply a reply to a request made at offset `askedAt`; false when stale. */
  apply(askedAt: number, r: LogReply, now = Date.now()): boolean {
    if (!this.log.apply(askedAt, r)) return false;
    if (r.log && this.firstLineAt === null) this.firstLineAt = now;
    const lines = this.log.buf.lines;
    const last = this.log.buf.partial || lines[lines.length - 1];
    if (last) this.lastLine = last;
    if (r.deployment) this.setRecord(r.deployment);
    else if (r.status && this.record && RANK[r.status] >= RANK[this.record.status]) this.setRecord({ ...this.record, status: r.status });
    return true;
  }

  /** A newer record replaces the one held; an older one (a slow reply) never moves status back. */
  setRecord(d: Deployment) {
    if (this.record && this.record.id === d.id && RANK[d.status] < RANK[this.record.status]) return;
    this.record = d;
    this.see(d);
  }

  private see(d: Deployment) {
    if (d.status === "building" || d.started_at) this.reached.building = true;
    if (d.status === "deploying" || d.status === "done") this.reached.building = this.reached.deploying = true;
    // A failure after the image was resolved happened while rolling out.
    if (d.status === "failed" && d.image) this.reached.building = this.reached.deploying = true;
  }

  /**
   * What an event on the feed means for deployment `id` of `app`: "line"
   * (a log line: pull, and show it as the latest line at once), "state" (a
   * status change: pull), or null (not ours).
   */
  onEvent(message: string, app: string, id: number): "line" | "state" | null {
    const line = deploymentLine(message, app, id);
    if (line !== null) {
      this.lastLine = line;
      return "line";
    }
    return concernsDeployment(message, app, id) ? "state" : null;
  }
}

/** The pipeline's middle step, named for what this deployment does there. */
export function buildStepLabel(d: Pick<Deployment, "rollback_of" | "commit" | "image">, git: boolean): string {
  if (d.rollback_of) return "Restore";
  return git ? "Build" : "Pull";
}

/** `app NAME: deployment N queued by BY` -> {app, id, by}. */
export function queuedEvent(message: string): { app: string; id: number; by: string } | null {
  const m = message.match(/^app ([^:]+): deployment (\d+) queued by (.+)$/);
  return m ? { app: m[1], id: Number(m[2]), by: m[3] } : null;
}

// Deployments this browser tab started: their "queued" event needs no toast.
const mine = new Set<string>();
export const markMine = (org: string, app: string, id: number) => mine.add(`${org}/${app}/${id}`);
export const isMine = (org: string, app: string, id: number) => mine.has(`${org}/${app}/${id}`);
