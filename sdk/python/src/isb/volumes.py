"""Named custom storage volumes.

    import isb
    await isb.volumes.create("cache", config={"size": "10GiB"})
    vols = await isb.volumes.list()

`pool` defaults to `auto` on the server (incus-zfs, else default, else the
first pool); `list` without a pool covers all pools.
"""

from __future__ import annotations

import builtins
from typing import Any, Dict, Mapping, Optional

from ._client import Client, default_client
from ._types import VolumeInfo


def _c(client: Optional[Client]) -> Client:
    return client if client is not None else default_client()


async def list(pool: Optional[str] = None, *, client: Optional[Client] = None) -> builtins.list[VolumeInfo]:  # noqa: A001
    """Volumes in `pool`, or in every pool."""
    r = await _c(client).call("volume.list", {"pool": pool})
    return [VolumeInfo.from_json(v) for v in r]


async def get(name: str, pool: Optional[str] = None, *, client: Optional[Client] = None) -> VolumeInfo:
    """One volume. Raises NotFoundError."""
    return VolumeInfo.from_json(await _c(client).call("volume.get", {"name": name, "pool": pool}))


async def create(
    name: str,
    pool: Optional[str] = None,
    *,
    config: Optional[Mapping[str, Any]] = None,
    client: Optional[Client] = None,
) -> Dict[str, Any]:
    """Create a volume if missing. Returns `{"created": bool, "pool": str}`."""
    params: Dict[str, Any] = {"name": name, "pool": pool}
    if config:
        params["config"] = {str(k): str(v) for k, v in config.items()}
    r = await _c(client).call("volume.create", params)
    return dict(r)


async def remove(name: str, pool: Optional[str] = None, *, client: Optional[Client] = None) -> None:
    """Delete a volume. Refused (ApiError) while a sandbox uses it."""
    await _c(client).call("volume.remove", {"name": name, "pool": pool})


class Volumes:
    """The same operations bound to one client: `Volumes(client).list()`."""

    def __init__(self, client: Optional[Client] = None) -> None:
        self.client = _c(client)

    async def list(self, pool: Optional[str] = None) -> builtins.list[VolumeInfo]:
        return await list(pool, client=self.client)

    async def get(self, name: str, pool: Optional[str] = None) -> VolumeInfo:
        return await get(name, pool, client=self.client)

    async def create(
        self, name: str, pool: Optional[str] = None, *, config: Optional[Mapping[str, Any]] = None
    ) -> Dict[str, Any]:
        return await create(name, pool, config=config, client=self.client)

    async def remove(self, name: str, pool: Optional[str] = None) -> None:
        await remove(name, pool, client=self.client)
