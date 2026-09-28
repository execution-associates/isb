"""Hatch build hook: bundle a static isb binary into a platform wheel.

    ISB_WHEEL_BINARY=/path/to/isb \
    ISB_WHEEL_PLAT=manylinux_2_17_x86_64.musllinux_1_2_x86_64 \
    uv build --wheel

copies the binary to `isb/_bin/isb` (mode 755) and tags the wheel
`py3-none-<ISB_WHEEL_PLAT>`. The binary is static musl, so it runs on any
Linux of that architecture. Without ISB_WHEEL_BINARY the wheel is pure and
finds isb through ISB_BIN or PATH.
"""

from __future__ import annotations

import os
import shutil
import tempfile
from typing import Any

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class IsbBinaryHook(BuildHookInterface):  # type: ignore[type-arg]
    PLUGIN_NAME = "custom"

    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        if self.target_name != "wheel":
            return
        src = os.environ.get("ISB_WHEEL_BINARY")
        if not src:
            return
        plat = os.environ.get("ISB_WHEEL_PLAT")
        if not plat:
            raise RuntimeError(
                "ISB_WHEEL_BINARY is set but ISB_WHEEL_PLAT is not (e.g. manylinux_2_17_x86_64.musllinux_1_2_x86_64)"
            )
        if not os.path.isfile(src):
            raise RuntimeError(f"ISB_WHEEL_BINARY={src} is not a file")
        self._tmp = tempfile.mkdtemp(prefix="isb-wheel-")
        dst = os.path.join(self._tmp, "isb")
        shutil.copyfile(src, dst)
        os.chmod(dst, 0o755)
        build_data["force_include"][dst] = "isb/_bin/isb"
        build_data["pure_python"] = False
        build_data["tag"] = f"py3-none-{plat}"

    def finalize(self, version: str, build_data: dict[str, Any], artifact_path: str) -> None:
        tmp = getattr(self, "_tmp", None)
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)
