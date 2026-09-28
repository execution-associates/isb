"""The protocol client: one long-lived `isb rpc` subprocess per Client."""

from __future__ import annotations

import asyncio
import itertools
import json
import os
import shutil
from pathlib import Path
from types import TracebackType
from typing import Any, Callable, Dict, List, Mapping, Optional, Tuple, Type, Union

from ._errors import BinaryNotFoundError, IsbError, ProcessError, ProtocolError, error_from_json
from ._util import Duration, duration

PROTOCOL = 1
"""The protocol version this SDK speaks."""

EventHandler = Callable[[str, Any], None]
"""Called with (event name, data) for each event line of a request."""

# One reply can carry a whole exec's output (base64), so lines may be large.
_LINE_LIMIT = 1 << 30
_STDERR_TAIL = 16 * 1024
_HELLO_TIMEOUT = 30.0


def find_binary(isb_bin: Union[str, "os.PathLike[str]", None] = None) -> str:
    """Locate the isb binary: `isb_bin`, else `$ISB_BIN`, else the binary bundled
    in this package (`isb/_bin/isb`), else `isb` on PATH."""
    if isb_bin:
        return os.fspath(isb_bin)
    env = os.environ.get("ISB_BIN")
    if env:
        return env
    bundled = Path(__file__).resolve().parent / "_bin" / "isb"
    if bundled.is_file() and os.access(bundled, os.X_OK):
        return str(bundled)
    found = shutil.which("isb")
    if found:
        return found
    raise BinaryNotFoundError(
        "binary_not_found",
        "no isb binary: pass isb_bin=, set ISB_BIN, install a wheel that bundles it, or put isb on PATH",
    )


class _Pending:
    __slots__ = ("future", "method", "on_event")

    def __init__(self, future: "asyncio.Future[Any]", on_event: Optional[EventHandler], method: str) -> None:
        self.future = future
        self.on_event = on_event
        self.method = method


class Client:
    """A connection to `isb rpc`.

    The subprocess starts on first use and lives until `close()`. Requests run
    concurrently over it; replies and events are matched by request id.

    `socket`, `project` and `create_timeout` become the global flags
    `--socket`, `--project` and `--create-timeout` of `isb rpc`.
    """

    def __init__(
        self,
        isb_bin: Union[str, "os.PathLike[str]", None] = None,
        socket: Optional[str] = None,
        project: Optional[str] = None,
        create_timeout: Optional[Duration] = None,
    ) -> None:
        self._isb_bin = isb_bin
        self.socket = socket
        self.project = project
        self.create_timeout = create_timeout
        self._proc: Optional[asyncio.subprocess.Process] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._reader: Optional["asyncio.Task[None]"] = None
        self._stderr_task: Optional["asyncio.Task[None]"] = None
        self._stderr = bytearray()
        self._pending: Dict[int, _Pending] = {}
        self._ids = itertools.count(1)
        self._start_lock: Optional[asyncio.Lock] = None
        self._write_lock: Optional[asyncio.Lock] = None
        self._hello: Optional[Dict[str, Any]] = None
        self._dead: Optional[IsbError] = None
        self._closed = False

    # -- lifecycle ---------------------------------------------------------

    def argv(self) -> List[str]:
        """The command line this client runs."""
        argv = [find_binary(self._isb_bin)]
        if self.socket:
            argv += ["--socket", str(self.socket)]
        if self.project:
            argv += ["--project", self.project]
        ct = duration(self.create_timeout)
        if ct:
            argv += ["--create-timeout", ct]
        return [*argv, "rpc"]

    @property
    def server_version(self) -> Optional[str]:
        """The isb version from the server's hello line (None before start)."""
        return None if self._hello is None else str(self._hello.get("isb"))

    @property
    def closed(self) -> bool:
        return self._closed

    @property
    def running(self) -> bool:
        """True while the subprocess is up and answering."""
        return self._proc is not None and self._dead is None and not self._closed

    async def start(self) -> None:
        """Start the subprocess now instead of on the first request."""
        if self._closed:
            raise ProcessError("process", "client is closed")
        loop = asyncio.get_running_loop()
        if self._loop is not None and self._loop is not loop:
            raise ProcessError(
                "process",
                "this Client was started on another event loop; create one Client per loop",
            )
        if self._start_lock is None:
            self._start_lock = asyncio.Lock()
        async with self._start_lock:
            if self._dead is not None:
                raise self._dead
            if self._proc is not None:
                return
            await self._spawn(loop)

    async def _spawn(self, loop: asyncio.AbstractEventLoop) -> None:
        argv = self.argv()
        try:
            proc = await asyncio.create_subprocess_exec(
                *argv,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                limit=_LINE_LIMIT,
            )
        except OSError as e:
            raise ProcessError("process", f"cannot start {argv[0]}: {e}") from e
        assert proc.stdout is not None and proc.stderr is not None
        self._loop = loop
        self._stderr_task = loop.create_task(self._read_stderr(proc.stderr))
        try:
            line = await asyncio.wait_for(proc.stdout.readline(), _HELLO_TIMEOUT)
        except asyncio.TimeoutError:
            await self._kill(proc)
            raise ProcessError("process", f"no hello from {' '.join(argv)} within {_HELLO_TIMEOUT:g}s") from None
        if not line:
            code = await self._reap(proc)
            raise ProcessError(
                "process",
                f"{' '.join(argv)} exited (status {code}) before its hello line; "
                f"does this isb have the `rpc` command?{self._stderr_suffix()}",
                {"stderr": self._stderr_text(), "exit_code": code},
            )
        try:
            hello = json.loads(line)
            proto = hello["protocol"]
        except (ValueError, KeyError, TypeError):
            await self._kill(proc)
            raise ProtocolError("protocol", f"unexpected hello line from isb: {line[:200]!r}") from None
        if proto != PROTOCOL:
            await self._kill(proc)
            raise ProtocolError(
                "protocol",
                f"isb {hello.get('isb')} speaks protocol {proto}; this SDK speaks {PROTOCOL}",
                {"hello": hello},
            )
        self._hello = hello
        self._proc = proc
        self._write_lock = asyncio.Lock()
        self._reader = loop.create_task(self._read_loop(proc))

    async def close(self, timeout: float = 10.0) -> None:
        """Close stdin so the server exits, wait up to `timeout` seconds, then kill it.

        Pending requests fail with ProcessError."""
        if self._closed:
            return
        self._closed = True
        proc = self._proc
        if proc is None:
            if self._stderr_task is not None:
                self._stderr_task.cancel()
            return
        try:
            if proc.stdin is not None and not proc.stdin.is_closing():
                proc.stdin.close()
        except Exception:  # noqa: BLE001
            pass
        try:
            await asyncio.wait_for(proc.wait(), timeout)
        except asyncio.TimeoutError:
            await self._kill(proc)
        if self._reader is not None:
            try:
                await asyncio.wait_for(self._reader, 5)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                pass
        self._fail_all(ProcessError("process", "client closed"))
        if self._stderr_task is not None:
            self._stderr_task.cancel()

    async def __aenter__(self) -> "Client":
        await self.start()
        return self

    async def __aexit__(
        self,
        exc_type: Optional[Type[BaseException]],
        exc: Optional[BaseException],
        tb: Optional[TracebackType],
    ) -> None:
        await self.close()

    async def _kill(self, proc: asyncio.subprocess.Process) -> None:
        try:
            if proc.stdin is not None:
                proc.stdin.close()
        except Exception:  # noqa: BLE001
            pass
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        try:
            await asyncio.wait_for(proc.wait(), 5)
        except asyncio.TimeoutError:
            pass

    async def _reap(self, proc: asyncio.subprocess.Process) -> Optional[int]:
        try:
            if proc.stdin is not None:
                proc.stdin.close()
        except Exception:  # noqa: BLE001
            pass
        code: Optional[int]
        try:
            code = await asyncio.wait_for(proc.wait(), 5)
        except asyncio.TimeoutError:
            await self._kill(proc)
            code = proc.returncode
        if self._stderr_task is not None:
            try:
                await asyncio.wait_for(asyncio.shield(self._stderr_task), 1)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                pass
        return code

    async def _read_stderr(self, stream: asyncio.StreamReader) -> None:
        while True:
            chunk = await stream.read(4096)
            if not chunk:
                return
            self._stderr += chunk
            if len(self._stderr) > _STDERR_TAIL:
                del self._stderr[: len(self._stderr) - _STDERR_TAIL]

    def _stderr_text(self) -> str:
        return self._stderr.decode(errors="replace").strip()

    def _stderr_suffix(self) -> str:
        t = self._stderr_text()
        return f"\nstderr: {t}" if t else ""

    async def _read_loop(self, proc: asyncio.subprocess.Process) -> None:
        assert proc.stdout is not None
        reason: Optional[IsbError] = None
        try:
            while True:
                line = await proc.stdout.readline()
                if not line:
                    break
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                if isinstance(msg, dict):
                    self._dispatch(msg)
        except asyncio.CancelledError:
            reason = ProcessError("process", "client closed")
            raise
        except Exception as e:  # noqa: BLE001
            reason = ProcessError("process", f"reading from isb rpc failed: {e}")
        finally:
            if reason is None:
                if self._closed:
                    reason = ProcessError("process", "client closed")
                else:
                    code = await self._reap(proc)
                    reason = ProcessError(
                        "process",
                        f"isb rpc exited (status {code}){self._stderr_suffix()}",
                        {"stderr": self._stderr_text(), "exit_code": code},
                    )
            self._dead = reason
            self._fail_all(reason)

    def _dispatch(self, msg: Dict[str, Any]) -> None:
        rid = msg.get("id")
        if not isinstance(rid, int):
            return  # `bad_request` with id null: nothing to match.
        p = self._pending.get(rid)
        if p is None:
            return  # the caller gave up (cancelled) on this request
        if "event" in msg:
            if p.on_event is not None:
                try:
                    p.on_event(str(msg["event"]), msg.get("data"))
                except Exception:  # noqa: BLE001
                    pass  # a callback must never take down the reader
            return
        del self._pending[rid]
        if p.future.done():
            return
        if "error" in msg:
            p.future.set_exception(error_from_json(msg["error"]))
        else:
            p.future.set_result(msg.get("result"))

    def _fail_all(self, err: IsbError) -> None:
        pending, self._pending = self._pending, {}
        for p in pending.values():
            if not p.future.done():
                p.future.set_exception(type(err)(err.code, f"{p.method}: {err.message}", err.data))

    # -- requests ----------------------------------------------------------

    async def send(
        self,
        method: str,
        params: Optional[Mapping[str, Any]] = None,
        *,
        on_event: Optional[EventHandler] = None,
    ) -> Tuple[int, "asyncio.Future[Any]"]:
        """Send a request and return its id and a future for the final result.

        Most callers want `call()`; this is for requests that are driven while
        they run (streaming exec)."""
        if self._proc is None or self._loop is not asyncio.get_running_loop():
            await self.start()
        if self._dead is not None:
            raise self._dead
        if self._closed:
            raise ProcessError("process", "client is closed")
        proc = self._proc
        assert proc is not None and proc.stdin is not None and self._write_lock is not None
        rid = next(self._ids)
        req: Dict[str, Any] = {"id": rid, "method": method}
        if params:
            req["params"] = {k: v for k, v in params.items() if v is not None}
        fut: "asyncio.Future[Any]" = asyncio.get_running_loop().create_future()
        self._pending[rid] = _Pending(fut, on_event, method)
        data = (json.dumps(req, separators=(",", ":")) + "\n").encode()
        try:
            async with self._write_lock:
                proc.stdin.write(data)
                await proc.stdin.drain()
        except (BrokenPipeError, ConnectionResetError, RuntimeError) as e:
            self._pending.pop(rid, None)
            if self._dead is not None:
                raise self._dead from e
            raise ProcessError("process", f"{method}: cannot write to isb rpc: {e}") from e
        return rid, fut

    async def call(
        self,
        method: str,
        params: Optional[Mapping[str, Any]] = None,
        *,
        on_event: Optional[EventHandler] = None,
    ) -> Any:
        """Send a request and wait for its result. Raises the mapped IsbError.

        Params whose value is None are omitted. If the caller is cancelled the
        request keeps running in the server and its reply is ignored."""
        rid, fut = await self.send(method, params, on_event=on_event)
        try:
            return await fut
        finally:
            if not fut.done():
                self._pending.pop(rid, None)

    async def version(self) -> Dict[str, Any]:
        """`{isb, protocol}` of the server."""
        r = await self.call("version")
        return dict(r)

    async def schema(self) -> Dict[str, Any]:
        """The JSON Schema of the compose format."""
        r = await self.call("schema")
        return dict(r)


_default: Optional[Client] = None


def default_client() -> Client:
    """The module-level client used when a function gets no `client=`.

    Created lazily with default settings (binary from ISB_BIN, the bundled
    binary, or PATH). A new one is made when the previous one was closed or
    belongs to another event loop."""
    global _default
    try:
        loop: Optional[asyncio.AbstractEventLoop] = asyncio.get_running_loop()
    except RuntimeError:
        loop = None
    c = _default
    if c is None or c.closed or (loop is not None and c._loop is not None and c._loop is not loop):
        c = _default = Client()
    return c


def set_default_client(client: Optional[Client]) -> None:
    """Replace the module-level default client (None resets it)."""
    global _default
    _default = client
