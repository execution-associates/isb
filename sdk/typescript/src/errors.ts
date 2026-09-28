/**
 * Errors. Every failure reported by `isb rpc` becomes an {@link IsbError}
 * (or a subclass chosen by its `code`); failures of the subprocess itself
 * are {@link ProcessExitedError} and {@link ClientClosedError}.
 */

/** The error object of the protocol: `{code, message, data?}`. */
export interface RpcErrorObject {
  code: string;
  message: string;
  data?: Record<string, unknown> | null;
}

export class IsbError extends Error {
  /** Stable error code from docs/rpc.md (`not_found`, `connect`, ...). */
  readonly code: string;
  /** Extra fields for some codes (`socket`, `timeout_secs`, ...). */
  readonly data: Record<string, unknown> | undefined;

  constructor(code: string, message: string, data?: Record<string, unknown> | null) {
    super(message);
    this.name = new.target.name;
    this.code = code;
    this.data = data ?? undefined;
  }
}

/** Sandbox, volume, device or running exec missing (also incusd 404). */
export class NotFoundError extends IsbError {}
/** Name taken (also incusd 409). */
export class AlreadyExistsError extends IsbError {}
/** A readiness check did not pass in time, or the instance stopped. */
export class NotReadyError extends IsbError {}
/** `request_timeout`, `operation_timeout` or `exec_timeout`. */
export class IsbTimeoutError extends IsbError {}
/** `invalid`, `interpolation` or `parse`: bad params, spec or compose file. */
export class InvalidError extends IsbError {}
/** isb cannot reach incusd. */
export class ConnectError extends IsbError {}
/** `protocol`, `bad_request` or `websocket`; also an unknown method or a bad hello. */
export class ProtocolError extends IsbError {}
/** The `isb rpc` subprocess exited (or could not start) with requests pending. */
export class ProcessExitedError extends IsbError {}
/** The client was closed; no further requests are accepted. */
export class ClientClosedError extends IsbError {}

type ErrorClass = new (
  code: string,
  message: string,
  data?: Record<string, unknown> | null,
) => IsbError;

const BY_CODE: Record<string, ErrorClass> = {
  not_found: NotFoundError,
  already_exists: AlreadyExistsError,
  not_ready: NotReadyError,
  request_timeout: IsbTimeoutError,
  operation_timeout: IsbTimeoutError,
  exec_timeout: IsbTimeoutError,
  invalid: InvalidError,
  interpolation: InvalidError,
  parse: InvalidError,
  connect: ConnectError,
  protocol: ProtocolError,
  bad_request: ProtocolError,
  websocket: ProtocolError,
};

/** Build the right error class for a protocol error object. */
export function errorFromRpc(e: RpcErrorObject): IsbError {
  const cls = BY_CODE[e.code] ?? IsbError;
  return new cls(e.code, e.message, e.data);
}
