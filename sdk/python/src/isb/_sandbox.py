"""Sandbox handles, exec, and streaming exec."""

from __future__ import annotations

import asyncio
import os
import signal as _signal
from types import TracebackType
from typing import (
    TYPE_CHECKING,
    Any,
    AsyncIterator,
    Awaitable,
    Callable,
    Dict,
    List,
    Literal,
    Mapping,
    Optional,
    Protocol,
    Sequence,
    Type,
    Union,
    overload,
)

from ._client import Client, EventHandler, default_client
from ._errors import NotFoundError, ProcessError
from ._spec import NamedVolumeSpec, PortSpec, ReadyCheck, SandboxSpec
from ._types import ApplyReport, ExecDefaults, ExecEvent, ExecOutput, Plan, PruneResult, SandboxInfo
from ._util import Duration, b64decode, b64encode, duration

if TYPE_CHECKING:
    from typing_extensions import Unpack

    from ._spec import SandboxSpecFields

ProgressCallback = Callable[[str], None]
"""Receives each progress line (`"web: creating from dev-base"`)."""

Cmd = Union[str, Sequence[str]]
StdinData = Union[bytes, bytearray, memoryview, str]
Labels = Union[Mapping[str, Optional[str]], Sequence[str], None]


def _client(client: Optional[Client]) -> Client:
    return client if client is not None else default_client()


def _progress(on_progress: Optional[ProgressCallback]) -> Optional[EventHandler]:
    if on_progress is None:
        return None
    cb = on_progress

    def handler(event: str, data: Any) -> None:
        if event == "progress":
            cb(str(data))

    return handler


def _build_spec(
    name: Optional[str],
    image: Optional[str],
    spec: Optional[Mapping[str, Any]],
    fields: Mapping[str, Any],
) -> Dict[str, Any]:
    d: Dict[str, Any] = dict(spec or {})
    d.update(fields)
    if name is not None:
        d["container_name"] = name
    if image is not None:
        d["image"] = image
    return d


def _spec_params(
    spec: Dict[str, Any],
    base_dir: Union[str, "os.PathLike[str]", None],
    named_volumes: Optional[Mapping[str, NamedVolumeSpec]],
) -> Dict[str, Any]:
    # The server's working directory is fixed when it starts; anchor relative
    # bind paths at the caller's current directory instead.
    p: Dict[str, Any] = {"spec": spec, "base_dir": os.fspath(base_dir) if base_dir is not None else os.getcwd()}
    if named_volumes:
        p["volumes"] = dict(named_volumes)
    return p


def _env_map(env: Any) -> Dict[str, Any]:
    """An environment given as a map or as docker's `KEY=VALUE` list, as a map."""
    if not env:
        return {}
    if isinstance(env, Mapping):
        return dict(env)
    out: Dict[str, Any] = {}
    for item in env:
        k, sep, v = str(item).partition("=")
        if not sep:
            raise ValueError(f"environment entry {k!r} has no value: write {k}=VALUE")
        out[k] = v
    return out


def _exec_defaults(spec: Mapping[str, Any]) -> Optional[ExecDefaults]:
    """The exec defaults a spec implies: `user`, `working_dir` and `exec`."""
    d: ExecDefaults = {}
    if spec.get("user") is not None:
        d["user"] = spec["user"]
    if spec.get("working_dir") is not None:
        d["cwd"] = spec["working_dir"]
    ex = spec.get("exec") or {}
    env = _env_map(ex.get("env"))
    if env:
        d["env"] = env
    if ex.get("login") is not None:
        d["login"] = ex["login"]
    return d or None


def _argv(cmd: Cmd, args: Optional[Sequence[str]]) -> List[str]:
    if isinstance(cmd, str):
        return [cmd, *(args or [])]
    if args is not None:
        raise TypeError("pass either a program and args, or a full argv list as cmd, not both")
    argv = list(cmd)
    if not argv:
        raise ValueError("empty argv")
    return argv


def _labels(labels: Labels) -> List[str]:
    if labels is None:
        return []
    if isinstance(labels, Mapping):
        return [k if v is None else f"{k}={v}" for k, v in labels.items()]
    if isinstance(labels, str):
        return [labels]
    return list(labels)


class _RemoveByName(Protocol):
    def __call__(self, name: str, force: bool = False, *, client: Optional[Client] = None) -> Awaitable[None]: ...


class _RemoveSelf(Protocol):
    def __call__(self, force: bool = False) -> Awaitable[None]: ...


class _Remove:
    """`Sandbox.remove(name, force)` on the class, `sb.remove(force)` on an instance."""

    @overload
    def __get__(self, obj: None, owner: Any) -> _RemoveByName: ...
    @overload
    def __get__(self, obj: "Sandbox", owner: Any) -> _RemoveSelf: ...
    def __get__(self, obj: Optional["Sandbox"], owner: Any) -> Union[_RemoveByName, _RemoveSelf]:
        if obj is None:

            async def by_name(name: str, force: bool = False, *, client: Optional[Client] = None) -> None:
                await _client(client).call("sandbox.remove", {"name": name, "force": force})

            return by_name
        sb = obj

        async def remove_self(force: bool = False) -> None:
            await sb.client.call("sandbox.remove", {"name": sb.name, "force": force})

        return remove_self


class Sandbox:
    """A handle on one sandbox (an incus instance).

    Get one from `create`, `connect_or_create`, `get`, or `Project.sandbox`.
    A handle from create/connect_or_create/compose carries the exec defaults
    its spec implies (`user`, `working_dir`, `exec.env`, `exec.login`), which
    `exec` sends along."""

    def __init__(
        self,
        name: str,
        *,
        client: Optional[Client] = None,
        spec: Optional[Mapping[str, Any]] = None,
        exec_defaults: Optional[ExecDefaults] = None,
    ) -> None:
        self.name = name
        self.client = _client(client)
        self.spec: Optional[Dict[str, Any]] = dict(spec) if spec is not None else None
        if exec_defaults is None and spec is not None:
            exec_defaults = _exec_defaults(spec)
        self.exec_defaults: Optional[ExecDefaults] = exec_defaults
        self.last_info: Optional[SandboxInfo] = None
        self.last_report: Optional[ApplyReport] = None

    def __repr__(self) -> str:
        return f"Sandbox({self.name!r})"

    # -- constructors ------------------------------------------------------

    @classmethod
    async def create(
        cls,
        name: str,
        *,
        image: Optional[str] = None,
        client: Optional[Client] = None,
        base_dir: Union[str, "os.PathLike[str]", None] = None,
        wait_ready: bool = True,
        named_volumes: Optional[Mapping[str, NamedVolumeSpec]] = None,
        on_progress: Optional[ProgressCallback] = None,
        spec: Optional[SandboxSpec] = None,
        **fields: "Unpack[SandboxSpecFields]",
    ) -> "Sandbox":
        """Create a sandbox. Fails with AlreadyExistsError if the name is taken.

        `name` is the incus instance name (the spec's `container_name`).
        `fields` are SandboxSpec fields (cpus, mem_limit, environment, volumes,
        ports, user, working_dir, ready, ...); `spec` may instead (or also)
        give a full spec dict, with `fields`, `name` and `image` taking
        precedence. `named_volumes` are top-level named volume definitions, as
        in a compose file's `volumes:`: a mount's `source` names a key, whose
        `name` is the incus volume (default: the key itself). Relative bind
        paths resolve against `base_dir` (default: cwd)."""
        c = _client(client)
        s = _build_spec(name, image, spec, fields)
        params = _spec_params(s, base_dir, named_volumes)
        params["wait_ready"] = wait_ready
        info = await c.call("sandbox.create", params, on_event=_progress(on_progress))
        sb = cls(name, client=c, spec=s)
        sb.last_info = SandboxInfo.from_json(info)
        return sb

    @classmethod
    async def connect_or_create(
        cls,
        name: str,
        *,
        image: Optional[str] = None,
        client: Optional[Client] = None,
        base_dir: Union[str, "os.PathLike[str]", None] = None,
        wait_ready: bool = True,
        prune_devices: bool = False,
        named_volumes: Optional[Mapping[str, NamedVolumeSpec]] = None,
        on_progress: Optional[ProgressCallback] = None,
        spec: Optional[SandboxSpec] = None,
        **fields: "Unpack[SandboxSpecFields]",
    ) -> "Sandbox":
        """Create the sandbox, or reconcile an existing one to the spec (only
        what differs is changed). The report is on `sandbox.last_report`."""
        c = _client(client)
        s = _build_spec(name, image, spec, fields)
        params = _spec_params(s, base_dir, named_volumes)
        params["wait_ready"] = wait_ready
        params["prune_devices"] = prune_devices
        r = await c.call("sandbox.ensure", params, on_event=_progress(on_progress))
        sb = cls(name, client=c, spec=s)
        sb.last_info = SandboxInfo.from_json(r["info"])
        sb.last_report = ApplyReport.from_json(r["report"])
        return sb

    ensure = connect_or_create

    @classmethod
    async def get(cls, name: str, *, client: Optional[Client] = None) -> "Sandbox":
        """A handle on an existing sandbox. Raises NotFoundError. The handle
        has no exec defaults."""
        c = _client(client)
        sb = cls(name, client=c)
        await sb.info()
        return sb

    @staticmethod
    async def plan(
        name: str,
        *,
        image: Optional[str] = None,
        client: Optional[Client] = None,
        base_dir: Union[str, "os.PathLike[str]", None] = None,
        prune_devices: bool = False,
        named_volumes: Optional[Mapping[str, NamedVolumeSpec]] = None,
        spec: Optional[SandboxSpec] = None,
        **fields: "Unpack[SandboxSpecFields]",
    ) -> Plan:
        """What connect_or_create would do, without doing it."""
        c = _client(client)
        params = _spec_params(_build_spec(name, image, spec, fields), base_dir, named_volumes)
        params["prune_devices"] = prune_devices
        return Plan.from_json(await c.call("sandbox.plan", params))

    @staticmethod
    async def resolve(
        name: str,
        *,
        image: Optional[str] = None,
        client: Optional[Client] = None,
        base_dir: Union[str, "os.PathLike[str]", None] = None,
        named_volumes: Optional[Mapping[str, NamedVolumeSpec]] = None,
        spec: Optional[SandboxSpec] = None,
        **fields: "Unpack[SandboxSpecFields]",
    ) -> Dict[str, Any]:
        """The spec resolved against this host (config keys, devices, pool, readiness)."""
        c = _client(client)
        params = _spec_params(_build_spec(name, image, spec, fields), base_dir, named_volumes)
        r = await c.call("sandbox.resolve", params)
        return dict(r)

    @staticmethod
    async def list_with(labels: Labels = None, *, client: Optional[Client] = None) -> List[SandboxInfo]:
        """Sandboxes carrying all the given labels: `{"app": "web", "tmp": None}`
        (None: any value) or `["app=web", "tmp"]`."""
        r = await _client(client).call("sandbox.list", {"labels": _labels(labels)})
        return [SandboxInfo.from_json(x) for x in r]

    @staticmethod
    async def list(*, client: Optional[Client] = None) -> List[SandboxInfo]:
        """All sandboxes (incus instances) in the project."""
        return await Sandbox.list_with(None, client=client)

    remove = _Remove()

    # -- instance ----------------------------------------------------------

    async def info(self) -> SandboxInfo:
        """Current state. Raises NotFoundError if the sandbox is gone."""
        info = SandboxInfo.from_json(await self.client.call("sandbox.get", {"name": self.name}))
        self.last_info = info
        return info

    async def labels(self) -> Dict[str, str]:
        return (await self.info()).labels

    async def start(self) -> None:
        """Start and wait until running (not the spec's other readiness checks)."""
        await self.client.call("sandbox.start", {"name": self.name})

    async def stop(self, force: bool = False, timeout: Optional[Duration] = "30s") -> None:
        await self.client.call("sandbox.stop", {"name": self.name, "force": force, "timeout": duration(timeout)})

    async def restart(self) -> None:
        await self.client.call("sandbox.restart", {"name": self.name})

    async def wait_ready(
        self,
        ready: Optional[Sequence[ReadyCheck]] = None,
        ready_timeout: Optional[Duration] = None,
    ) -> None:
        """Run readiness checks. Defaults: the spec's `ready`/`ready_timeout` if
        this handle has a spec, else the server's (`["running"]`, 60s)."""
        s = self.spec or {}
        if ready is None:
            ready = s.get("ready")
        if ready_timeout is None:
            ready_timeout = s.get("ready_timeout")
        await self.client.call(
            "sandbox.wait_ready",
            {
                "name": self.name,
                "ready": list(ready) if ready is not None else None,
                "ready_timeout": duration(ready_timeout),
                "exec": dict(self.exec_defaults) if self.exec_defaults else None,
            },
        )

    async def add_port(self, port: PortSpec) -> str:
        """Add a proxy device (leaves a correct one alone). `port` is a `ports`
        entry: `"8080:80"`, `PortBinding.publish(...)`, `PortBinding.host(...)`
        or `PortBinding.guest(...)`. Returns the listen address in use."""
        r = await self.client.call(
            "sandbox.add_port", {"name": self.name, "port": port if isinstance(port, str) else dict(port)}
        )
        return str(r["listen"])

    async def remove_device(self, device: str) -> bool:
        """Remove an instance-local device. False if it was not there."""
        r = await self.client.call("sandbox.remove_device", {"name": self.name, "device": device})
        return bool(r["removed"])

    def _exec_params(
        self,
        cmd: Cmd,
        args: Optional[Sequence[str]],
        cwd: Optional[str],
        user: Union[str, int, None],
        env: Optional[Mapping[str, Any]],
        login: Optional[bool],
        timeout: Optional[Duration],
        tty: bool,
        width: Optional[int],
        height: Optional[int],
    ) -> Dict[str, Any]:
        p: Dict[str, Any] = {"name": self.name, "argv": _argv(cmd, args)}
        if self.exec_defaults:
            p["defaults"] = dict(self.exec_defaults)
        p["cwd"] = cwd
        p["user"] = None if user is None else str(user)
        p["env"] = {str(k): str(v) for k, v in env.items()} if env else None
        p["login"] = login
        p["timeout"] = duration(timeout)
        if tty:
            p["tty"] = True
        p["width"] = width
        p["height"] = height
        return p

    async def exec(
        self,
        cmd: Cmd,
        args: Optional[Sequence[str]] = None,
        *,
        cwd: Optional[str] = None,
        user: Union[str, int, None] = None,
        env: Optional[Mapping[str, Any]] = None,
        login: Optional[bool] = None,
        timeout: Optional[Duration] = None,
        stdin: Optional[StdinData] = None,
        tty: bool = False,
        width: Optional[int] = None,
        height: Optional[int] = None,
    ) -> ExecOutput:
        """Run argv (never through a shell) and capture its output.

        `cmd` is a program with `args`, or a full argv list. `stdin` is sent
        as the command's input, then EOF. `timeout` (seconds or `"90s"`) kills
        the command and raises IsbTimeoutError. A non-zero exit is not an
        error: check `exit_code` / `success`."""
        p = self._exec_params(cmd, args, cwd, user, env, login, timeout, tty, width, height)
        if stdin is not None:
            p["stdin"] = {"data": b64encode(stdin)}
        r = await self.client.call("sandbox.exec", p)
        return ExecOutput(int(r["exit_code"]), b64decode(r.get("stdout")), b64decode(r.get("stderr")))

    async def exec_stream(
        self,
        cmd: Cmd,
        args: Optional[Sequence[str]] = None,
        *,
        cwd: Optional[str] = None,
        user: Union[str, int, None] = None,
        env: Optional[Mapping[str, Any]] = None,
        login: Optional[bool] = None,
        timeout: Optional[Duration] = None,
        stdin: Union[Literal["piped"], StdinData, None] = None,
        tty: bool = False,
        width: Optional[int] = None,
        height: Optional[int] = None,
    ) -> "ExecProcess":
        """Start argv and stream its output as it is produced.

        `stdin`: None (no input), `"piped"` (write with `ExecProcess.write`,
        end with `close_stdin`), or bytes/str sent up front."""
        p = self._exec_params(cmd, args, cwd, user, env, login, timeout, tty, width, height)
        p["stream"] = True
        if stdin is None:
            pass
        elif isinstance(stdin, str) and stdin == "piped":
            p["stdin"] = "piped"
        else:
            p["stdin"] = {"data": b64encode(stdin)}
        proc = ExecProcess(self.client)
        rid, fut = await self.client.send("sandbox.exec", p, on_event=proc._on_event)
        proc._attach(rid, fut)
        return proc


_END = object()


class ExecProcess:
    """A running streaming exec.

    Iterate it (`async for ev in proc`) for ExecEvent chunks in order, drive
    it with write/close_stdin/signal/resize, and `await proc.wait()` for the
    exit code. As an async context manager it kills the command (SIGKILL) on
    exit if it is still running."""

    def __init__(self, client: Client) -> None:
        self._client = client
        self._queue: "asyncio.Queue[object]" = asyncio.Queue()
        self._id: Optional[int] = None
        self._fut: Optional["asyncio.Future[Any]"] = None
        self._ended = False

    @property
    def id(self) -> int:
        """The request id of the exec, used by the control methods."""
        assert self._id is not None
        return self._id

    @property
    def done(self) -> bool:
        return self._fut is not None and self._fut.done()

    def _attach(self, rid: int, fut: "asyncio.Future[Any]") -> None:
        self._id = rid
        self._fut = fut
        fut.add_done_callback(lambda _f: self._queue.put_nowait(_END))

    def _on_event(self, event: str, data: Any) -> None:
        if event in ("stdout", "stderr"):
            self._queue.put_nowait(ExecEvent(event, b64decode(data)))  # type: ignore[arg-type]

    def __aiter__(self) -> AsyncIterator[ExecEvent]:
        return self

    async def __anext__(self) -> ExecEvent:
        if self._ended:
            raise StopAsyncIteration
        item = await self._queue.get()
        if item is _END:
            self._ended = True
            assert self._fut is not None
            exc = self._fut.exception()
            if exc is not None:
                raise exc
            raise StopAsyncIteration
        assert isinstance(item, ExecEvent)
        return item

    async def wait(self) -> int:
        """Wait for the command to exit and return its exit code. Output not
        yet iterated stays available to iterate."""
        assert self._fut is not None
        r = await asyncio.shield(self._fut)
        return int(r["exit_code"])

    async def collect(self) -> ExecOutput:
        """Consume the remaining output and wait: stdout and stderr joined."""
        out, err = bytearray(), bytearray()
        async for ev in self:
            (out if ev.kind == "stdout" else err).extend(ev.data)
        return ExecOutput(await self.wait(), bytes(out), bytes(err))

    async def _control(self, method: str, params: Dict[str, Any]) -> None:
        # The server queues control calls that arrive before the exec starts;
        # not_found means the exec has finished.
        await self._client.call(method, {"exec": self.id, **params})

    async def write(self, data: StdinData) -> None:
        """Write to the command's stdin (needs `stdin="piped"`)."""
        await self._control("exec.write", {"data": b64encode(data)})

    async def close_stdin(self) -> None:
        """Send EOF on stdin."""
        await self._control("exec.close_stdin", {})

    async def signal(self, sig: Union[int, _signal.Signals]) -> None:
        """Send a signal (a number such as 15, or `signal.SIGTERM`)."""
        await self._control("exec.signal", {"signal": int(sig)})

    async def resize(self, width: int, height: int) -> None:
        """Resize the TTY (tty execs only)."""
        await self._control("exec.resize", {"width": width, "height": height})

    async def kill(self) -> None:
        """SIGKILL the command, ignoring an exec that already finished."""
        if self.done:
            return
        try:
            await self.signal(_signal.SIGKILL)
        except (NotFoundError, ProcessError):
            pass

    async def __aenter__(self) -> "ExecProcess":
        return self

    async def __aexit__(
        self,
        exc_type: Optional[Type[BaseException]],
        exc: Optional[BaseException],
        tb: Optional[TracebackType],
    ) -> None:
        if not self.done:
            await self.kill()
            assert self._fut is not None
            try:
                await asyncio.wait_for(asyncio.shield(self._fut), 30)
            except BaseException:  # noqa: BLE001
                pass
        elif self._fut is not None and not self._fut.cancelled():
            self._fut.exception()  # mark retrieved


async def prune(label: str, dry_run: bool = True, *, client: Optional[Client] = None) -> List[PruneResult]:
    """Sandboxes whose `label` value is a host path that no longer exists;
    deleted unless `dry_run` (the default)."""
    r = await _client(client).call("prune", {"label": label, "dry_run": dry_run})
    return [PruneResult.from_json(x) for x in r]
