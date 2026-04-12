"""Apply SODP delta ops to an in-memory state value.

Each op is a dict with keys ``op``, ``path``, and (for ADD/UPDATE) ``value``.
Paths follow JSON Pointer (RFC 6901):

* ``"/"``   — the root value itself
* ``"/x"``  — top-level field ``x`` on an object
* ``"/x/y"`` — nested field
* ``"/-"``  — the element past the end of an array (append)
* ``"/3"``  — index ``3`` into an array
"""

from __future__ import annotations

import copy
from typing import Any

_KNOWN_OPS = frozenset({"ADD", "UPDATE", "REMOVE"})


def apply_ops(state: Any, ops: list[dict]) -> Any:
    """Return the new state after applying *ops* in order. Does not mutate *state*.

    Raises :class:`ValueError` if any op has an unknown ``op`` type (anything
    other than ``"ADD"``, ``"UPDATE"``, or ``"REMOVE"``). This is deliberate —
    silently ignoring unknown ops causes the client's cached state to diverge
    from the server, which is far worse than a loud failure.
    """
    for op in ops:
        state = _apply_one(state, op)
    return state


def _apply_one(state: Any, op: dict) -> Any:
    kind: str = op.get("op", "")
    if kind not in _KNOWN_OPS:
        raise ValueError(
            f"[SODP] unknown delta op type: {kind!r}. "
            f"Expected one of: ADD, UPDATE, REMOVE"
        )

    path: str = op.get("path", "/")
    parts = _parse_path(path)

    # Root operation.
    if not parts:
        return None if kind == "REMOVE" else op.get("value")

    # Deep-clone the container so callers keep immutability guarantees.
    # Non-container state at a non-root path is coerced to an empty dict.
    if isinstance(state, (dict, list)):
        root: Any = copy.deepcopy(state)
    else:
        root = {}

    # Walk to the parent node, materialising intermediate dicts as needed.
    node: Any = root
    for key in parts[:-1]:
        if isinstance(node, list):
            idx = _as_index(key)
            if idx is None or not (0 <= idx < len(node)):
                # Non-indexable segment on a list — nothing we can sensibly do.
                return root
            node = node[idx]
        else:
            child = node.get(key) if isinstance(node, dict) else None
            if not isinstance(child, (dict, list)):
                child = {}
                if isinstance(node, dict):
                    node[key] = child
            node = child

    last = parts[-1]

    # RFC 6901 "-" append on an array.
    if last == "-" and isinstance(node, list):
        if kind in ("ADD", "UPDATE"):
            node.append(op.get("value"))
        elif kind == "REMOVE" and node:
            node.pop()
        return root

    if isinstance(node, list):
        idx = _as_index(last)
        if idx is None:
            return root
        if kind in ("ADD", "UPDATE"):
            if 0 <= idx < len(node):
                node[idx] = op.get("value")
            elif idx == len(node):
                node.append(op.get("value"))
        elif kind == "REMOVE" and 0 <= idx < len(node):
            node.pop(idx)
        return root

    # Dict branch.
    if not isinstance(node, dict):
        return root
    if kind in ("ADD", "UPDATE"):
        node[last] = op.get("value")
    elif kind == "REMOVE":
        node.pop(last, None)

    return root


def _as_index(seg: str) -> int | None:
    """Parse *seg* as a non-negative array index, or return ``None``."""
    if not seg or not seg.isdigit():
        return None
    return int(seg)


def _parse_path(path: str) -> list[str]:
    """``"/"`` → ``[]``, ``"/x/y"`` → ``["x", "y"]``, ``"/-"`` → ``["-"]``."""
    if path == "/":
        return []
    return path[1:].split("/")
