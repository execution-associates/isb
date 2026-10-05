// The shared event connection: resuming, and what happens across a daemon restart.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Hub, type LiveEvent } from "./live";

class FakeSource {
  static all: FakeSource[] = [];
  onopen: (() => void) | null = null;
  closed = false;
  private handlers = new Map<string, ((ev: Event) => void)[]>();
  constructor(public url: string) {
    FakeSource.all.push(this);
  }
  addEventListener(name: string, f: (ev: Event) => void) {
    this.handlers.set(name, [...(this.handlers.get(name) ?? []), f]);
  }
  close() {
    this.closed = true;
  }
  open() {
    this.onopen?.();
  }
  send(e: Partial<LiveEvent> & { seq: number }) {
    const m = new MessageEvent(e.level ?? "info", { data: JSON.stringify({ level: "info", stack: "s", message: "", at: 0, ...e }) });
    for (const f of this.handlers.get(e.level ?? "info") ?? []) f(m);
  }
  fail() {
    for (const f of this.handlers.get("error") ?? []) f(new Event("error"));
  }
}

describe("Hub", () => {
  beforeEach(() => {
    FakeSource.all = [];
    vi.stubGlobal("EventSource", FakeSource);
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("resumes after the last event, and takes a restarted daemon's lower numbers", () => {
    const hub = new Hub();
    const got: number[] = [];
    let reopened = 0;
    hub.onReopen(() => reopened++);
    hub.subscribe((e) => got.push(e.seq));

    const first = FakeSource.all[0];
    expect(first.url).toBe("/api/v1/events");
    first.open();
    first.send({ seq: 499 });
    first.send({ seq: 500 });
    expect(reopened).toBe(0);

    // The daemon restarts: the stream drops and comes back.
    first.fail();
    expect(hub.state).toBe("reconnecting");
    vi.advanceTimersByTime(2000);
    const second = FakeSource.all[1];
    expect(second.url).toBe("/api/v1/events?since=500");
    second.open();
    expect(reopened).toBe(1);
    expect(hub.state).toBe("live");
    // The new process numbers from 1 and answers the old cursor from the start.
    second.send({ seq: 1 });
    second.send({ seq: 2 });
    expect(got).toEqual([499, 500, 1, 2]);

    second.fail();
    vi.advanceTimersByTime(2000);
    expect(FakeSource.all[2].url).toBe("/api/v1/events?since=2");
  });
});
