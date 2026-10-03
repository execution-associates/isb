"""Result types. Plain dataclasses built from the protocol's JSON."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, List, Literal, Mapping, Optional, TypedDict, Union

from ._spec import BoolOrString, Scalar

Action = Dict[str, Any]
"""One plan step: an object tagged by `action` (`create_volume`, `create_instance`,
`set_config`, `add_device`, `replace_device`, `remove_device`, `start_instance`,
`add_port`, `fix_owner`, `note`). See docs/reference/rpc.md."""


class ExecDefaults(TypedDict, total=False):
    """Defaults for exec into a sandbox; per-call options override them.

    A spec implies them: `user`, `working_dir` (as `cwd`), `exec.env` and
    `exec.login`."""

    user: Union[str, int]
    """Guest user: a name (`dev`), `uid`, `uid:gid` or `name:group`."""
    cwd: str
    """Working directory in the guest."""
    env: Mapping[str, Scalar]
    """Environment for exec (merged over the instance `environment`)."""
    login: BoolOrString
    """Run argv through the user's login shell."""


def _strmap(v: Any) -> Dict[str, str]:
    return {str(k): str(x) for k, x in (v or {}).items()}


@dataclass
class SandboxInfo:
    """A sandbox as listed by incus."""

    name: str
    status: str
    type: str
    labels: Dict[str, str] = field(default_factory=dict)
    """`user.*` config keys without the prefix (isb's own `user.isb.*` excluded)."""
    config: Dict[str, str] = field(default_factory=dict)
    devices: Dict[str, Dict[str, str]] = field(default_factory=dict)
    """Instance-local devices, each a map of string properties."""
    profiles: List[str] = field(default_factory=list)
    created_at: str = ""
    description: str = ""

    @property
    def running(self) -> bool:
        return self.status.lower() == "running"

    @classmethod
    def from_json(cls, v: Dict[str, Any]) -> "SandboxInfo":
        return cls(
            name=v["name"],
            status=v.get("status", ""),
            type=v.get("type", ""),
            labels=_strmap(v.get("labels")),
            config=_strmap(v.get("config")),
            devices={k: _strmap(d) for k, d in (v.get("devices") or {}).items()},
            profiles=list(v.get("profiles") or []),
            created_at=v.get("created_at", ""),
            description=v.get("description", ""),
        )


@dataclass
class ApplyReport:
    """What an ensure (connect_or_create, compose up) did."""

    name: str
    created: bool
    applied: List[Action] = field(default_factory=list)
    ports: Dict[str, str] = field(default_factory=dict)
    """Device name to the listen address in use, for ports published from a range."""
    restart_needed: List[str] = field(default_factory=list)
    """Config keys changed that take effect only after a restart."""

    @property
    def changed(self) -> bool:
        """True if anything other than a note was applied."""
        return any(a.get("action") != "note" for a in self.applied)

    @classmethod
    def from_json(cls, v: Dict[str, Any]) -> "ApplyReport":
        return cls(
            name=v["name"],
            created=bool(v.get("created")),
            applied=list(v.get("applied") or []),
            ports=_strmap(v.get("ports")),
            restart_needed=list(v.get("restart_needed") or []),
        )


@dataclass
class Plan:
    """What an ensure would do. `status` is None when the sandbox does not exist."""

    name: str
    status: Optional[str]
    actions: List[Action] = field(default_factory=list)

    @property
    def is_noop(self) -> bool:
        """No changes (notes only)."""
        return all(a.get("action") == "note" for a in self.actions)

    @classmethod
    def from_json(cls, v: Dict[str, Any]) -> "Plan":
        return cls(name=v["name"], status=v.get("status"), actions=list(v.get("actions") or []))


@dataclass
class VolumeInfo:
    """A custom storage volume."""

    name: str
    pool: str
    content_type: str = ""
    config: Dict[str, str] = field(default_factory=dict)
    used_by: List[str] = field(default_factory=list)

    @classmethod
    def from_json(cls, v: Dict[str, Any]) -> "VolumeInfo":
        return cls(
            name=v["name"],
            pool=v.get("pool", ""),
            content_type=v.get("content_type", ""),
            config=_strmap(v.get("config")),
            used_by=list(v.get("used_by") or []),
        )


@dataclass
class PruneResult:
    """A sandbox whose label pointed at a vanished host path."""

    name: str
    path: str
    deleted: bool

    @classmethod
    def from_json(cls, v: Dict[str, Any]) -> "PruneResult":
        return cls(name=v["name"], path=v.get("path", ""), deleted=bool(v.get("deleted")))


@dataclass
class ExecOutput:
    """The result of a captured exec."""

    exit_code: int
    stdout: bytes
    stderr: bytes

    @property
    def success(self) -> bool:
        return self.exit_code == 0

    @property
    def stdout_text(self) -> str:
        return self.stdout.decode(errors="replace")

    @property
    def stderr_text(self) -> str:
        return self.stderr.decode(errors="replace")


@dataclass
class ExecEvent:
    """A chunk of output from a streaming exec. With a TTY, everything is `stdout`."""

    kind: Literal["stdout", "stderr"]
    data: bytes

    @property
    def text(self) -> str:
        return self.data.decode(errors="replace")
