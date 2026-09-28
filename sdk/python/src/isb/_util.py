from __future__ import annotations

import base64
from typing import Optional, Union

Duration = Union[int, float, str]
"""A duration: seconds as a number, or a string such as `"90s"`, `"5m"`, `"1500ms"`."""


def duration(value: Optional[Duration]) -> Optional[str]:
    """Convert a duration to the protocol's string form."""
    if value is None:
        return None
    if isinstance(value, bool):
        raise TypeError("a duration cannot be a bool")
    if isinstance(value, (int, float)):
        if value < 0:
            raise ValueError(f"negative duration: {value}")
        ms = round(value * 1000)
        return f"{ms}ms"
    return str(value)


def b64encode(data: Union[bytes, bytearray, memoryview, str]) -> str:
    if isinstance(data, str):
        data = data.encode()
    return base64.b64encode(bytes(data)).decode("ascii")


def b64decode(data: object) -> bytes:
    if not isinstance(data, str) or not data:
        return b""
    return base64.b64decode(data)
