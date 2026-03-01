"""Apply SODP delta ops to an in-memory state value.

Each op is a dict with keys ``op``, ``path``, and (for ADD/UPDATE) ``value``.
Paths follow JSON-pointer syntax: ``"/"`` is the root, ``"/x/y"`` is a nested field.
"""

from __future__ import annotations
from typing import Any


def apply_ops(state: Any, ops: list[dict]) -> Any:
    """Return the new state after applying *ops* in order.  Does not mutate *state*."""
    for op in ops:
        state = _apply_one(state, op)
    return state


def _apply_one(state: Any, op: dict) -> Any:
    path: str = op.get("path", "/")
    kind: str = op.get("op", "")
    parts = _parse_path(path)

    # Root operation.
    if not parts:
        return None if kind == "REMOVE" else op.get("value")

    # Clone the root dict so callers keep immutability guarantees.
    root: dict = dict(state) if isinstance(state, dict) else {}

    # Walk to the parent, materialising intermediate dicts as needed.
    node = root
    for key in parts[:-1]:
        child = node.get(key)
        if not isinstance(child, dict):
            child = {}
        node[key] = child = dict(child)
        node = child

    last = parts[-1]
    if kind in ("ADD", "UPDATE"):
        node[last] = op.get("value")
    elif kind == "REMOVE":
        node.pop(last, None)

    return root


def _parse_path(path: str) -> list[str]:
    """``"/"`` → ``[]``,  ``"/x/y"`` → ``["x", "y"]``."""
    if path == "/":
        return []
    return path[1:].split("/")
