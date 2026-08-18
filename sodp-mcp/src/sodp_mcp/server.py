"""SODP MCP server — exposes SODP state operations as AI agent tools."""

from __future__ import annotations

import asyncio
import json
import os
from contextlib import asynccontextmanager
from typing import Any

from mcp.server.fastmcp import FastMCP
from sodp.client import SodpClient

# ── Client lifecycle ──────────────────────────────────────────────────────────

_client: SodpClient | None = None


def _get() -> SodpClient:
    if _client is None:
        raise RuntimeError("SODP client not initialised — is the server running?")
    return _client


@asynccontextmanager
async def _lifespan(server: FastMCP):
    global _client
    url = os.environ.get("SODP_URL", "ws://localhost:7777")
    token = os.environ.get("SODP_TOKEN") or None
    _client = SodpClient(url, token=token)
    try:
        await asyncio.wait_for(_client.ready, timeout=10.0)
    except asyncio.TimeoutError:
        raise RuntimeError(f"Could not connect to SODP server at {url} within 10 s")
    yield
    _client.close()
    _client = None


# ── Server ────────────────────────────────────────────────────────────────────

mcp = FastMCP(
    "sodp",
    instructions=(
        "Tools for interacting with a SODP (State-Oriented Data Protocol) server. "
        "SODP stores named state keys whose values are JSON objects. "
        "Clients subscribe once and receive only the changed fields as deltas. "
        "Use sodp_read to inspect state, sodp_watch to observe live changes, "
        "and sodp_set / sodp_patch / sodp_set_in to write state."
    ),
    lifespan=_lifespan,
)

# ── Read tools ────────────────────────────────────────────────────────────────


@mcp.tool()
async def sodp_read(key: str) -> str:
    """Return the current state value for a SODP key.

    Subscribes to the key, waits for the server's STATE_INIT snapshot,
    then unsubscribes. Returns the value, its version, and whether it has
    ever been written (initialized).

    Returns JSON: {"key", "value", "version", "initialized"}
    """
    client = _get()
    ready = asyncio.Event()
    result: dict[str, Any] = {}

    def on_update(value: Any, meta: Any) -> None:
        result.update(
            key=key,
            value=value,
            version=meta.version,
            initialized=meta.initialized,
        )
        ready.set()

    unsub = client.watch(key, on_update)
    try:
        await asyncio.wait_for(ready.wait(), timeout=10.0)
    except asyncio.TimeoutError:
        result = {"key": key, "value": None, "version": 0, "initialized": False}
    finally:
        unsub()

    return json.dumps(result)


@mcp.tool()
async def sodp_watch(key: str, duration: float = 3.0) -> str:
    """Watch a SODP key and collect all updates for a given duration.

    Returns the initial snapshot plus every delta received during the window.
    Useful for observing real-time state changes before deciding what to write.

    Args:
        key:      State key to observe (e.g. "game.score", "room.players").
        duration: Seconds to collect updates (default 3, max 30).

    Returns JSON: {"key", "updates": [{"value", "version", "initialized", "source"}, ...]}
    """
    client = _get()
    duration = max(0.1, min(duration, 30.0))
    updates: list[dict[str, Any]] = []

    def on_update(value: Any, meta: Any) -> None:
        updates.append(
            {
                "value": value,
                "version": meta.version,
                "initialized": meta.initialized,
                "source": meta.source,
            }
        )

    unsub = client.watch(key, on_update)
    await asyncio.sleep(duration)
    unsub()

    return json.dumps({"key": key, "updates": updates})


# ── Write tools ───────────────────────────────────────────────────────────────


@mcp.tool()
async def sodp_set(key: str, value: Any) -> str:
    """Replace the full state for a SODP key.

    All watchers receive a delta containing only the fields that changed.
    Creates the key if it does not exist.

    Args:
        key:   State key (e.g. "game.score", "user.profile").
        value: New value — any JSON-serializable type (dict, list, number, string).
    """
    await _get().set(key, value)
    return f"OK: {key} updated."


@mcp.tool()
async def sodp_patch(key: str, partial: dict) -> str:
    """Shallow-merge fields into an existing SODP state key.

    Only the specified fields are changed; all other fields are preserved.
    The key must already exist on the server.

    Args:
        key:     State key to patch.
        partial: Dict of fields to update (e.g. {"health": 80, "score": 42}).
    """
    await _get().patch(key, partial)
    return f"OK: {key} patched."


@mcp.tool()
async def sodp_set_in(key: str, path: str, value: Any) -> str:
    """Set a single nested field within a SODP state key.

    Uses a JSON Pointer path to target the field atomically.
    Only that field changes; the rest of the object is untouched.

    Args:
        key:   State key (e.g. "game.state").
        path:  JSON Pointer to the target field (e.g. "/player/health", "/scores/0").
        value: Value to write at that path.
    """
    await _get().state(key).set_in(path, value)
    return f"OK: {key}{path} set."


@mcp.tool()
async def sodp_delete(key: str) -> str:
    """Delete a SODP state key from the server.

    All watchers receive a REMOVE delta. The key is gone until written again.

    Args:
        key: State key to delete.
    """
    await _get().state(key).delete()
    return f"OK: {key} deleted."


@mcp.tool()
async def sodp_presence(key: str, path: str, value: Any) -> str:
    """Set a presence entry that is auto-removed when this session disconnects.

    Use for live cursors, active-user indicators, or any ephemeral per-session
    state. The server broadcasts a REMOVE delta to all watchers on disconnect,
    eliminating ghost entries.

    Args:
        key:   State key holding presence data (e.g. "collab.cursors").
        path:  JSON Pointer for this session's slot (e.g. "/user_abc123").
        value: Presence payload (e.g. {"name": "Alice", "line": 42}).
    """
    await _get().presence(key, path, value)
    return f"OK: presence set at {key}{path}."


# ── Entry point ───────────────────────────────────────────────────────────────


def main() -> None:
    mcp.run()


if __name__ == "__main__":
    main()
