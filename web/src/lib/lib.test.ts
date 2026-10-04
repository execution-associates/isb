import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, requestInit } from "@/api/client";
import { backoff, eventsUrl } from "@/api/events";
import { describeAgent, initials, relativeTime } from "@/lib/format";
import { errorMessage, passwordProblem, safeNext, signInErrorMessage } from "@/lib/messages";
import { b64urlToBytes, bytesToB64url } from "@/lib/webauthn";
import { eventOrg } from "@/pages/org";

describe("safeNext matches the server's rule", () => {
  it.each([
    ["/", "/"],
    ["/orgs/ocai", "/orgs/ocai"],
    ["/account?tab=tokens", "/account?tab=tokens"],
  ])("keeps %s", (n, want) => expect(safeNext(n)).toBe(want));
  it.each([null, "", "https://evil.example", "//evil.example", "/\\evil", "/a b", "/a\tb", "relative", "/x\u0000"])(
    "refuses %j",
    (n) => expect(safeNext(n)).toBe("/"),
  );
});

describe("sign-in error codes", () => {
  it("has plain words for every documented code", () => {
    const codes = [
      "unverified_email",
      "signup_closed",
      "invitation_mismatch",
      "invalid_invitation",
      "account_disabled",
      "setup_required",
      "identity_taken",
      "state_invalid",
      "state_mismatch",
      "provider_denied",
      "provider_error",
      "provider_unavailable",
      "unknown_provider",
      "invalid_request",
      "forbidden",
      "rate_limited",
      "internal",
    ];
    const fallback = signInErrorMessage("nope");
    for (const c of codes) {
      const m = signInErrorMessage(c);
      expect(m, c).not.toBe(fallback);
      expect(m, c).not.toContain("_");
    }
  });
  it("explains API errors", () => {
    expect(errorMessage(new ApiError(401, "invalid_credentials", "invalid email or password"))).toMatch(/don't match/);
    expect(errorMessage(new ApiError(429, "rate_limited", "slow down", undefined, 90))).toBe(
      "Too many attempts. Try again in 2 minutes.",
    );
    expect(errorMessage(new ApiError(400, "invalid", "password too short"))).toBe("Password too short.");
  });
});

describe("passwords", () => {
  it("counts characters, not UTF-16 units", () => {
    expect(passwordProblem("short")).toMatch(/12/);
    expect(passwordProblem("twelve chars")).toBeNull();
    expect(passwordProblem("😀".repeat(11))).toMatch(/12/);
  });
});

describe("base64url", () => {
  it("round-trips", () => {
    const b = new Uint8Array([0, 1, 2, 250, 251, 252, 253, 254, 255]);
    const s = bytesToB64url(b);
    expect(s).not.toMatch(/[+/=]/);
    expect(Array.from(b64urlToBytes(s))).toEqual(Array.from(b));
  });
});

describe("events", () => {
  it("backs off to 30s with bounded jitter", () => {
    expect(backoff(0, () => 0)).toBe(1000);
    expect(backoff(1, () => 0)).toBe(2000);
    expect(backoff(10, () => 0)).toBe(30_000);
    expect(backoff(10, () => 1)).toBe(36_000);
  });
  it("resumes after the last event", () => {
    expect(eventsUrl(0)).toBe("/api/v1/events");
    expect(eventsUrl(42)).toBe("/api/v1/events?since=42");
  });
  it("maps stacks to orgs", () => {
    expect(eventOrg("web")).toBe("default");
    expect(eventOrg("ocai/web")).toBe("ocai");
  });
});

describe("api client", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends the CSRF header on writes only", () => {
    const h = (m: string) => requestInit(m).headers as Record<string, string>;
    expect(h("GET")["X-Isb-Csrf"]).toBeUndefined();
    for (const m of ["POST", "PUT", "DELETE"]) expect(h(m)["X-Isb-Csrf"]).toBe("1");
    expect(requestInit("POST").credentials).toBe("same-origin");
  });

  it("turns error bodies into ApiError", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "conflict", message: "last way in" }), { status: 409 })),
    );
    await expect(api("DELETE", "/x")).rejects.toMatchObject({ status: 409, code: "conflict", message: "last way in" });
  });

  it("handles 204 and plain-text errors", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })));
    await expect(api("POST", "/x")).resolves.toBeUndefined();
    vi.stubGlobal("fetch", vi.fn(async () => new Response("not found\n", { status: 404 })));
    await expect(api("GET", "/x")).rejects.toMatchObject({ status: 404, code: "http_404", message: "not found" });
  });
});

describe("format", () => {
  it("relative times", () => {
    const now = 1_000_000_000_000;
    expect(relativeTime(null, now)).toBe("never");
    expect(relativeTime(now / 1000 - 3, now)).toBe("just now");
    expect(relativeTime(now / 1000 - 7200, now)).toMatch(/2 hours ago/);
  });
  it("initials and agents", () => {
    expect(initials("Ada Lovelace", "a@x.io")).toBe("AL");
    expect(initials("", "grace.hopper@x.io")).toBe("GH");
    expect(describeAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15")).toBe(
      "Safari on macOS",
    );
  });
});
