"""Errors raised by the SDK, mapped from the protocol's stable error codes."""

from __future__ import annotations

from typing import Any, Dict, Optional, Type


class IsbError(Exception):
    """Base class. `code` is the protocol's stable error code (docs/reference/rpc.md)."""

    code: str
    message: str
    data: Optional[Dict[str, Any]]

    def __init__(self, code: str, message: str, data: Optional[Dict[str, Any]] = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data

    def __repr__(self) -> str:
        return f"{type(self).__name__}(code={self.code!r}, message={self.message!r}, data={self.data!r})"


class NotFoundError(IsbError):
    """A sandbox, volume, device or running exec does not exist (`not_found`)."""


class AlreadyExistsError(IsbError):
    """The name is taken (`already_exists`)."""


class NotReadyError(IsbError):
    """A readiness check did not pass in time, or the instance stopped (`not_ready`)."""


class IsbTimeoutError(IsbError):
    """`request_timeout`, `operation_timeout` or `exec_timeout`."""


class InvalidError(IsbError):
    """Bad params, spec or argument, including `interpolation` and `parse` failures."""


class ConnectError(IsbError):
    """isb cannot reach incusd (`connect`)."""


class ApiError(IsbError):
    """incusd returned an error (`api`), or an incus operation failed (`operation_failed`)."""


class ProtocolError(IsbError):
    """The protocol itself failed: unknown method (`protocol`), a malformed line
    (`bad_request`), or an unexpected hello from the server."""


class ProcessError(IsbError):
    """The `isb rpc` subprocess could not be started, or exited while requests
    were pending (code `process`). `data["stderr"]` holds the tail of its stderr."""


class BinaryNotFoundError(ProcessError):
    """No isb binary was found (code `binary_not_found`)."""


_BY_CODE: Dict[str, Type[IsbError]] = {
    "not_found": NotFoundError,
    "already_exists": AlreadyExistsError,
    "not_ready": NotReadyError,
    "request_timeout": IsbTimeoutError,
    "operation_timeout": IsbTimeoutError,
    "exec_timeout": IsbTimeoutError,
    "invalid": InvalidError,
    "interpolation": InvalidError,
    "parse": InvalidError,
    "connect": ConnectError,
    "api": ApiError,
    "operation_failed": ApiError,
    "protocol": ProtocolError,
    "bad_request": ProtocolError,
    "process": ProcessError,
    "binary_not_found": BinaryNotFoundError,
}


def error_from_json(err: Any) -> IsbError:
    """Build the right exception from a reply's `error` object."""
    if not isinstance(err, dict):
        return ProtocolError("protocol", f"malformed error object: {err!r}")
    code = str(err.get("code") or "unknown")
    message = str(err.get("message") or code)
    data = err.get("data")
    cls = _BY_CODE.get(code, IsbError)
    return cls(code, message, data if isinstance(data, dict) else None)
