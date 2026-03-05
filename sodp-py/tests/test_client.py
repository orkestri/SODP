"""Unit tests for SodpClient — no running server needed."""

import asyncio
import pytest
from sodp.client import SodpClient, StateRef, WatchMeta, _WatchEntry


# ── WatchMeta ───────────────────────────────────────────────────────────────────

def test_watch_meta_frozen():
    meta = WatchMeta(version=1, initialized=True)
    assert meta.version == 1
    assert meta.initialized is True
    with pytest.raises(AttributeError):
        meta.version = 2  # type: ignore[misc]


# ── WatchEntry ──────────────────────────────────────────────────────────────────

def test_watch_entry_defaults():
    entry = _WatchEntry(key="test")
    assert entry.version == 0
    assert entry.value is None
    assert entry.initialized is False
    assert len(entry.callbacks) == 0


# ── SodpClient offline behavior ────────────────────────────────────────────────
# SodpClient.__init__ calls asyncio.create_task, so it must be created
# inside a running event loop (i.e. in an async test function).

@pytest.fixture
async def client():
    """Create a client pointing at a non-existent server (reconnect=False)."""
    c = SodpClient("ws://127.0.0.1:19999", reconnect=False)
    yield c
    c.close()
    await asyncio.sleep(0.05)  # let task cancel


async def test_watch_registers_entry(client: SodpClient):
    unsub = client.watch("test.key", lambda v, m: None)
    assert client.is_watching("test.key")
    assert client.get("test.key") is None
    unsub()


async def test_unwatch_removes_key(client: SodpClient):
    client.watch("test.key", lambda v, m: None)
    assert client.is_watching("test.key")
    client.unwatch("test.key")
    assert not client.is_watching("test.key")


async def test_multiple_watches_same_key(client: SodpClient):
    unsub_a = client.watch("shared", lambda v, m: None)
    unsub_b = client.watch("shared", lambda v, m: None)
    assert client.is_watching("shared")
    unsub_a()
    assert client.is_watching("shared")  # callback B still active
    unsub_b()


async def test_state_ref_delegates(client: SodpClient):
    ref = client.state("game.player")
    assert isinstance(ref, StateRef)
    assert ref.key == "game.player"
    ref.watch(lambda v, m: None)
    assert client.is_watching("game.player")
    ref.unwatch()
    assert not client.is_watching("game.player")


async def test_get_returns_none_for_unwatched(client: SodpClient):
    assert client.get("never.watched") is None


async def test_close_sets_closed(client: SodpClient):
    client.close()
    assert client._closed is True


async def test_call_timeout():
    """Call to non-existent server should timeout."""
    client = SodpClient("ws://127.0.0.1:19999", reconnect=False)
    try:
        with pytest.raises((TimeoutError, ConnectionError, asyncio.CancelledError)):
            await asyncio.wait_for(
                client.call("state.set", {"state": "x", "value": 1}),
                timeout=2.0,
            )
    finally:
        client.close()
