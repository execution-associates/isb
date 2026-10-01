"""Compose projects (isb.yaml) over the protocol."""

from __future__ import annotations

import os
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple, Union

from ._client import Client
from ._errors import NotFoundError
from ._sandbox import ProgressCallback, Sandbox, _client, _progress
from ._spec import ComposeFile
from ._types import ApplyReport, Plan

PathArg = Union[str, "os.PathLike[str]"]
Paths = Union[PathArg, Sequence[PathArg], None]


def _paths(p: Paths) -> List[str]:
    if p is None:
        return []
    if isinstance(p, (str, os.PathLike)):
        p = [p]
    # The server's working directory is fixed when it starts; resolve relative
    # paths against the caller's current directory instead.
    return [os.path.abspath(os.fspath(x)) for x in p]


def _first_existing(names: Sequence[str]) -> Optional[str]:
    for name in names:
        path = os.path.join(os.getcwd(), name)
        if os.path.exists(path):
            return path
    return None


def _default_files() -> List[str]:
    """`isb.yaml` (else `isb.yml`) in the cwd, plus `isb.override.yaml` (else
    `isb.override.yml`) when present, as the CLI does."""
    main = _first_existing(("isb.yaml", "isb.yml"))
    if main is None:
        return [os.path.join(os.getcwd(), "isb.yaml")]
    override = _first_existing(("isb.override.yaml", "isb.override.yml"))
    return [main] if override is None else [main, override]


class Project:
    """A loaded compose file (or several, merged in order).

    `file` is the resolved file: interpolated, merged, with the project name
    and every service's `container_name` filled in. `up`/`plan`/`down` reload the files on
    the server each time, with the same variables."""

    def __init__(
        self,
        client: Client,
        load_params: Dict[str, Any],
        result: Mapping[str, Any],
    ) -> None:
        self.client = client
        self._params = load_params
        self.name: str = str(result["name"])
        self.base_dir: str = str(result.get("base_dir", ""))
        self.files: List[str] = [str(f) for f in result.get("files") or []]
        self.file: ComposeFile = result["file"]

    def __repr__(self) -> str:
        return f"Project({self.name!r}, services={self.services!r})"

    @classmethod
    async def load(
        cls,
        files: Paths = None,
        *,
        env_files: Paths = None,
        project_name: Optional[str] = None,
        vars: Optional[Mapping[str, str]] = None,  # noqa: A002 (the protocol's name)
        client: Optional[Client] = None,
    ) -> "Project":
        """Load compose files (default `./isb.yaml`, else `./isb.yml`, plus
        `isb.override.yaml` next to it when present; a `.env` next to the
        first file is read too).

        `vars` win over the environment of the isb process for `${VAR}`;
        `env_files` come after both."""
        c = _client(client)
        params: Dict[str, Any] = {"files": _paths(files) or _default_files()}
        if env_files is not None:
            params["env_files"] = _paths(env_files)
        if project_name is not None:
            params["project_name"] = project_name
        if vars:
            params["vars"] = {str(k): str(v) for k, v in vars.items()}
        r = await c.call("compose.load", params)
        return cls(c, params, r)

    @property
    def services(self) -> List[str]:
        """Service names (sorted, as the server returns them)."""
        return list((self.file.get("services") or {}).keys())

    def sandbox(self, service: str) -> Sandbox:
        """A handle on a service's sandbox, carrying its exec defaults
        (`user`, `working_dir`, `exec`)."""
        spec = (self.file.get("services") or {}).get(service)
        if spec is None:
            raise NotFoundError("not_found", f"no service {service!r} in project {self.name}")
        name = spec.get("container_name") or f"{self.name}-{service}"
        return Sandbox(name, client=self.client, spec=dict(spec))

    def _with(self, services: Optional[Sequence[str]], **extra: Any) -> Dict[str, Any]:
        p = dict(self._params)
        if services:
            p["services"] = list(services)
        p.update(extra)
        return p

    async def up(
        self,
        services: Optional[Sequence[str]] = None,
        *,
        prune_devices: bool = False,
        wait_ready: bool = True,
        on_progress: Optional[ProgressCallback] = None,
    ) -> List[Tuple[str, ApplyReport]]:
        """Create or reconcile the services (all when None). Returns
        (service, report) pairs."""
        r = await self.client.call(
            "compose.up",
            self._with(services, prune_devices=prune_devices, wait_ready=wait_ready),
            on_event=_progress(on_progress),
        )
        return [(str(x["service"]), ApplyReport.from_json(x["report"])) for x in r]

    async def plan(
        self,
        services: Optional[Sequence[str]] = None,
        *,
        prune_devices: bool = False,
    ) -> List[Plan]:
        """What `up` would change."""
        r = await self.client.call("compose.plan", self._with(services, prune_devices=prune_devices))
        return [Plan.from_json(x) for x in r]

    async def down(
        self,
        services: Optional[Sequence[str]] = None,
        *,
        volumes: bool = False,
        on_progress: Optional[ProgressCallback] = None,
    ) -> None:
        """Delete the services' sandboxes. With `volumes` and no service list,
        also the file's non-external named volumes."""
        await self.client.call(
            "compose.down",
            self._with(services, volumes=volumes),
            on_event=_progress(on_progress),
        )

    async def reload(self) -> "Project":
        """Load the files again (same variables) and return the new Project."""
        r = await self.client.call("compose.load", self._params)
        return Project(self.client, self._params, r)
