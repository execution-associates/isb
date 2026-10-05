"""Python SDK for isb: declarative incus sandboxes.

A thin asyncio client of `isb rpc` (docs/reference/rpc.md). Zero runtime dependencies.

    import asyncio, isb

    async def main():
        async with isb.Client() as client:
            sb = await isb.Sandbox.create("demo", image="dev-base", client=client)
            out = await sb.exec("uname", ["-a"])
            print(out.stdout_text)
            await sb.remove(force=True)

    asyncio.run(main())
"""

from . import volumes
from ._builders import PortBinding, Volume
from ._client import PROTOCOL, Client, EventHandler, default_client, find_binary, set_default_client
from ._errors import (
    AlreadyExistsError,
    ApiError,
    BinaryNotFoundError,
    ConnectError,
    InvalidError,
    IsbError,
    IsbTimeoutError,
    NotFoundError,
    NotReadyError,
    ProcessError,
    ProtocolError,
)
from ._project import Project
from ._sandbox import ExecProcess, ProgressCallback, Sandbox, prune
from ._spec import (
    Command,
    ComposeFile,
    ExecSpec,
    IdmapSpec,
    MapOrList,
    NamedVolumeSpec,
    PortMapping,
    PortSpec,
    ProxyPort,
    ReadyCheck,
    SandboxSpec,
    SandboxSpecFields,
    VolumeMount,
    VolumeSpec,
)
from ._types import (
    Action,
    ApplyReport,
    ExecDefaults,
    ExecEvent,
    ExecOutput,
    Plan,
    PruneResult,
    SandboxInfo,
    VolumeInfo,
)
from ._util import Duration
from .volumes import Volumes

__version__ = "1.6.10"

__all__ = [
    "PROTOCOL",
    "Action",
    "AlreadyExistsError",
    "ApiError",
    "ApplyReport",
    "BinaryNotFoundError",
    "Client",
    "Command",
    "ComposeFile",
    "ConnectError",
    "Duration",
    "EventHandler",
    "ExecDefaults",
    "ExecEvent",
    "ExecOutput",
    "ExecProcess",
    "ExecSpec",
    "IdmapSpec",
    "InvalidError",
    "IsbError",
    "IsbTimeoutError",
    "MapOrList",
    "NamedVolumeSpec",
    "NotFoundError",
    "NotReadyError",
    "Plan",
    "PortBinding",
    "PortMapping",
    "PortSpec",
    "ProcessError",
    "ProgressCallback",
    "Project",
    "ProtocolError",
    "ProxyPort",
    "PruneResult",
    "ReadyCheck",
    "Sandbox",
    "SandboxInfo",
    "SandboxSpec",
    "SandboxSpecFields",
    "Volume",
    "VolumeInfo",
    "VolumeMount",
    "VolumeSpec",
    "Volumes",
    "__version__",
    "default_client",
    "find_binary",
    "prune",
    "set_default_client",
    "volumes",
]
