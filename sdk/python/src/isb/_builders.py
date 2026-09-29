"""Builders for spec fragments. They return plain dicts (the spec TypedDicts)."""

from __future__ import annotations

from enum import Enum
from typing import Mapping, Optional, Union

from ._spec import PortSpec, Scalar, VolumeSpec


class NamedVolumeMode(str, Enum):
    """How a named volume mount treats a missing volume."""

    ENSURE_EXISTS = "ensure_exists"
    """Create the volume if it is missing (the default)."""
    EXISTING = "existing"
    """The volume must already exist (`external: true`)."""


class Volume:
    """Mount builders for the `volumes` spec field (keyed by guest path)."""

    def __init__(self) -> None:  # pragma: no cover
        raise TypeError("use Volume.bind(...) or Volume.named(...)")

    @staticmethod
    def bind(
        host_path: str,
        *,
        readonly: bool = False,
        device: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> VolumeSpec:
        """Bind-mount a host path. Relative paths resolve against `base_dir`
        (default: the current directory at the time of the call)."""
        v: VolumeSpec = {"bind": str(host_path)}
        if readonly:
            v["readonly"] = True
        if device is not None:
            v["device"] = device
        if options:
            v["options"] = dict(options)
        return v

    @staticmethod
    def named(
        name: str,
        *,
        mode: NamedVolumeMode = NamedVolumeMode.ENSURE_EXISTS,
        owner: Union[str, int, None] = None,
        readonly: bool = False,
        pool: Optional[str] = None,
        device: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> VolumeSpec:
        """Mount a named custom volume, chowning the mount point to `owner` if set."""
        v: VolumeSpec = {"named": name}
        if NamedVolumeMode(mode) is NamedVolumeMode.EXISTING:
            v["external"] = True
        if owner is not None:
            v["owner"] = owner
        if readonly:
            v["readonly"] = True
        if pool is not None:
            v["pool"] = pool
        if device is not None:
            v["device"] = device
        if options:
            v["options"] = dict(options)
        return v


class PortBinding:
    """Proxy device builders for the `ports` spec field."""

    def __init__(self) -> None:  # pragma: no cover
        raise TypeError("use PortBinding.host(...) or PortBinding.guest(...)")

    @staticmethod
    def host(
        listen: Union[str, int],
        connect: Union[str, int],
        *,
        name: Optional[str] = None,
        search: Optional[int] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> PortSpec:
        """Listen on the host, connect in the guest (publish a guest port).

        Addresses take Docker-style shorthand: `5173`, `"0.0.0.0:5173"`,
        `"5353/udp"`, or the full `"tcp:HOST:PORT"`; the protocol defaults to tcp
        and the host to 127.0.0.1. `search`: if the listen port is taken, try up
        to this many ports past it."""
        p: PortSpec = {"bind": "host", "listen": str(listen), "connect": str(connect)}
        if name is not None:
            p["name"] = name
        if search is not None:
            p["search"] = search
        if options:
            p["options"] = dict(options)
        return p

    @staticmethod
    def guest(
        listen: Union[str, int],
        connect: Union[str, int],
        *,
        name: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> PortSpec:
        """Listen in the guest, connect on the host (reach a host service).

        Addresses take the same shorthand as `host()`."""
        p: PortSpec = {"bind": "guest", "listen": str(listen), "connect": str(connect)}
        if name is not None:
            p["name"] = name
        if options:
            p["options"] = dict(options)
        return p
