// The one fetch wrapper: same-origin credentials, JSON in and out, and the
// anti-CSRF header on every state-changing request (docs/reference/identity-api.md#csrf).

export class ApiError extends Error {
  status: number;
  code: string;
  data: unknown;
  retryAfter: number | null;
  constructor(status: number, code: string, message: string, data?: unknown, retryAfter?: number | null) {
    super(message);
    this.status = status;
    this.code = code;
    this.data = data;
    this.retryAfter = retryAfter ?? null;
  }
}

export const CSRF_HEADER = "X-Isb-Csrf";

export function requestInit(method: string, body?: unknown): RequestInit {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET" && method !== "HEAD") headers[CSRF_HEADER] = "1";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  return {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : JSON.stringify(body),
  };
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, requestInit(method, body));
  } catch {
    throw new ApiError(0, "network", "Can't reach the isb server. Check your connection and try again.");
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  let json: unknown = undefined;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    // a plain-text answer (a proxy, or a 404 from the server itself)
  }
  if (!res.ok) {
    const j = (json ?? {}) as { error?: string; message?: string; data?: unknown };
    const retry = res.headers.get("Retry-After");
    throw new ApiError(
      res.status,
      j.error ?? `http_${res.status}`,
      j.message ?? (text.trim() || res.statusText),
      j.data,
      retry ? Number(retry) : null,
    );
  }
  return json as T;
}

export const get = <T>(path: string) => api<T>("GET", path);
export const post = <T>(path: string, body?: unknown) => api<T>("POST", path, body ?? {});
export const del = <T = void>(path: string) => api<T>("DELETE", path);
