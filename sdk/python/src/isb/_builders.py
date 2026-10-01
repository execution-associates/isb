"""Builders for spec fragments. They return plain dicts (the spec TypedDicts)."""

from __future__ import annotations

import os
from typing import Mapping, Optional, Tuple, Union

from ._spec import PortMapping, ProxyPort, Scalar, VolumeMount

PathArg = Union[str, "os.PathLike[str]"]


class Volume:
    """Mount builders for the `volumes` spec field: each returns one list entry
    in the long form (`{type, source, target, ...}`)."""

    def __init__(self) -> None:  # pragma: no cover
        raise TypeError("use Volume.bind(...) or Volume.named(...)")

    @staticmethod
    def bind(
        source: PathArg,
        target: str,
        *,
        read_only: bool = False,
        device: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> VolumeMount:
        """Bind-mount the host path `source` at `target` in the guest.

        Relative paths resolve against `base_dir` (default: the current
        directory at the time of the call); `~` expands."""
        v: VolumeMount = {"type": "bind", "source": os.fspath(source), "target": target}
        if read_only:
            v["read_only"] = True
        if device is not None:
            v["device"] = device
        if options:
            v["options"] = dict(options)
        return v

    @staticmethod
    def named(
        source: str,
        target: str,
        *,
        external: bool = False,
        owner: Union[str, int, None] = None,
        read_only: bool = False,
        pool: Optional[str] = None,
        device: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> VolumeMount:
        """Mount the named custom volume `source` at `target` in the guest.

        `source` is a key of the top-level volume definitions (`named_volumes`),
        whose `name` is the incus volume; without a definition it is the incus
        volume name itself. The volume is created if missing unless `external`.
        `owner` chowns the mount point to that guest user once attached."""
        v: VolumeMount = {"type": "volume", "source": source, "target": target}
        if external:
            v["external"] = True
        if owner is not None:
            v["owner"] = owner
        if read_only:
            v["read_only"] = True
        if pool is not None:
            v["pool"] = pool
        if device is not None:
            v["device"] = device
        if options:
            v["options"] = dict(options)
        return v


_DEFAULT_HOSTS = ("127.0.0.1", "0.0.0.0")


def _split_addr(addr: str) -> Optional[Tuple[str, Optional[str], int]]:
    """(protocol, host or None, port) of a single-port TCP/UDP address in any
    shorthand the server takes (`5173`, `HOST:5173`, `5173/udp`,
    `tcp:HOST:PORT`, `[::1]:5173`); None for anything else."""
    a = addr.strip()
    proto = "tcp"
    head, sep, rest = a.partition(":")
    if sep and head in ("tcp", "udp"):
        proto, a = head, rest
    else:
        body, sep, suffix = a.rpartition("/")
        if sep and suffix in ("tcp", "udp"):
            proto, a = suffix, body
    host: Optional[str] = None
    if a.startswith("["):
        end = a.find("]")
        if end < 0 or a[end + 1 : end + 2] != ":":
            return None
        host, port = a[: end + 1], a[end + 2 :]
    elif ":" in a:
        host, _, port = a.rpartition(":")
        if not host or ":" in host:
            return None
    else:
        port = a
    if not port.isdigit() or not 0 < int(port) <= 65535:
        return None
    return proto, host, int(port)


def _searched(
    listen: str,
    connect: str,
    search: int,
    name: Optional[str],
    options: Optional[Mapping[str, Scalar]],
) -> PortMapping:
    """A host-bound proxy with `search` as docker's long form with a published range."""
    lis = _split_addr(listen)
    con = _split_addr(connect)
    if lis is None or con is None or lis[0] != con[0] or (con[1] is not None and con[1] not in _DEFAULT_HOSTS):
        raise ValueError(
            f"search needs a single TCP or UDP listen port and a connect port on the guest's default "
            f"address with the same protocol (got listen={listen!r}, connect={connect!r}); "
            f'use PortBinding.publish("START-END", target) instead'
        )
    proto, host, lport = lis
    if search < 0 or lport + search > 65535:
        raise ValueError(f"search={search} from port {lport} leaves the port range")
    p: PortMapping = {"published": f"{lport}-{lport + search}" if search else lport, "target": con[2]}
    if host is not None and host != "127.0.0.1":
        p["host_ip"] = host.strip("[]")
    if proto != "tcp":
        p["protocol"] = proto
    if name is not None:
        p["name"] = name
    if options:
        p["options"] = dict(options)
    return p


class PortBinding:
    """Builders for the `ports` spec field: docker-style published ports, and
    incus proxies in either direction."""

    def __init__(self) -> None:  # pragma: no cover
        raise TypeError("use PortBinding.publish(...), PortBinding.host(...) or PortBinding.guest(...)")

    @staticmethod
    def publish(
        published: Union[int, str],
        target: Union[int, str],
        *,
        host_ip: Optional[str] = None,
        protocol: Optional[str] = None,
        name: Optional[str] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> PortMapping:
        """Publish guest port `target` on host port `published` (docker's long form).

        `published` may be a range (`"5173-5223"`) with a single `target`: the
        first free port in it is taken, and reported in `ApplyReport.ports`.
        `host_ip` defaults to 127.0.0.1, `protocol` to tcp."""
        p: PortMapping = {"published": published, "target": target}
        if host_ip is not None:
            p["host_ip"] = host_ip
        if protocol is not None:
            p["protocol"] = protocol
        if name is not None:
            p["name"] = name
        if options:
            p["options"] = dict(options)
        return p

    @staticmethod
    def host(
        listen: Union[str, int],
        connect: Union[str, int],
        *,
        name: Optional[str] = None,
        search: Optional[int] = None,
        options: Optional[Mapping[str, Scalar]] = None,
    ) -> Union[ProxyPort, PortMapping]:
        """Listen on the host, connect in the guest (an incus proxy).

        Addresses take Docker-style shorthand: `5173`, `"0.0.0.0:5173"`,
        `"5353/udp"`, or the full `"tcp:HOST:PORT"` / `"unix:PATH"`; the
        protocol defaults to tcp and the host to 127.0.0.1.

        `search`: if the listen port is taken, take the first free one up to
        this many past it. The port is then written as a published range
        (`publish("5173-5223", 5173)`), which needs a single TCP/UDP listen
        port and a connect port with no host other than the default; anything
        else raises ValueError."""
        if search is not None:
            return _searched(str(listen), str(connect), search, name, options)
        p: ProxyPort = {"bind": "host", "listen": str(listen), "connect": str(connect)}
        if name is not None:
            p["name"] = name
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
    ) -> ProxyPort:
        """Listen in the guest, connect on the host (reach a host service).

        Addresses take the same shorthand as `host()`."""
        p: ProxyPort = {"bind": "guest", "listen": str(listen), "connect": str(connect)}
        if name is not None:
            p["name"] = name
        if options:
            p["options"] = dict(options)
        return p
