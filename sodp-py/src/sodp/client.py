"""SODP Python client — asyncio + WebSocket + MessagePack."""

from __future__ import annotations

import asyncio
import logging
import random
import uuid
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable

import msgpack
import websockets

from .delta import apply_ops

logger = logging.getLogger(__name__)

# ── Frame type constants ───────────────────────────────────────────────────────

_HELLO      = 0x01
_WATCH      = 0x02
_STATE_INIT = 0x03
_DELTA      = 0x04
_CALL       = 0x05
_RESULT     = 0x06
_ERROR      = 0x07
_HEARTBEAT  = 0x09
_RESUME     = 0x0A
_AUTH       = 0x0B
_AUTH_OK    = 0x0C
_UNWATCH    = 0x0D

# ── Public types ───────────────────────────────────────────────────────────────

@dataclass(frozen=True)
class WatchMeta:
    """Metadata delivered alongside every watch callback invocation."""
    version:     int
    initialized: bool


WatchCallback = Callable[[Any, WatchMeta], None | Awaitable[None]]


@dataclass
class _WatchEntry:
    key:         str
    callbacks:   set[WatchCallback] = field(default_factory=set)
    version:     int  = 0
    value:       Any  = None
    initialized: bool = False


# ── SodpClient ─────────────────────────────────────────────────────────────────

class SodpClient:
    """
    Async client for the State-Oriented Data Protocol.

    Must be created from within a running event loop (i.e. inside an ``async``
    function or ``asyncio.run()``).

    Example::

        async def main():
            client = SodpClient("ws://localhost:7777", token=jwt)
            await client.ready

            player = client.state("game.player")
            player.watch(lambda v, m: print(v))

            await player.set({"name": "Alice", "health": 100})
            await asyncio.sleep(1)
            client.close()

        asyncio.run(main())
    """

    def __init__(
        self,
        url: str,
        *,
        token: str | None = None,
        token_provider: Callable[[], str | Awaitable[str]] | None = None,
        reconnect: bool = True,
        reconnect_delay: float = 1.0,
        max_reconnect_delay: float = 30.0,
        on_connect:    Callable[[], None] | None = None,
        on_disconnect: Callable[[], None] | None = None,
    ) -> None:
        self._url               = url
        self._token             = token
        self._token_provider    = token_provider
        self._reconnect         = reconnect
        self._reconnect_delay   = reconnect_delay
        self._max_reconnect_delay = max_reconnect_delay
        self._on_connect_cb     = on_connect
        self._on_disconnect_cb  = on_disconnect

        self._ws:            websockets.WebSocketClientProtocol | None = None
        self._authenticated: bool = False
        self._closed:        bool = False
        self._reconnect_attempts: int = 0

        self._ready_event = asyncio.Event()
        self._watches:      dict[str, _WatchEntry]       = {}
        self._stream_to_key: dict[int, str]              = {}
        self._pending_calls: dict[str, asyncio.Future]   = {}
        self._call_queue:   list[tuple[int, int, Any]]   = []

        self._next_stream_id = 10
        self._next_seq       = 0
        self._send_queue: asyncio.Queue[bytes] = asyncio.Queue()

        self._task = asyncio.create_task(self._run())

    # ── ready ─────────────────────────────────────────────────────────────────

    def __await__(self):
        """``await client`` waits until the client is connected and authenticated."""
        return self._ready_event.wait().__await__()

    @property
    def ready(self) -> Awaitable[None]:
        """Awaitable that resolves once the client is authenticated and ready."""
        return self._ready_event.wait()

    # ── Connection lifecycle ──────────────────────────────────────────────────

    def close(self) -> None:
        """Gracefully close the connection and stop reconnecting."""
        self._closed = True
        self._task.cancel()

    async def _run(self) -> None:
        while not self._closed:
            try:
                async with websockets.connect(
                    self._url,
                    max_size=None,        # server enforces max_frame_bytes
                    ping_interval=None,   # SODP uses its own HEARTBEAT frames
                ) as ws:
                    self._ws    = ws
                    self._reconnect_attempts = 0
                    self._stream_to_key = {}
                    self._next_stream_id = 10
                    self._next_seq       = 0

                    sender = asyncio.create_task(self._sender(ws))
                    try:
                        await self._recv_loop(ws)
                    finally:
                        sender.cancel()
                        self._ws            = None
                        self._authenticated = False
                        for fut in self._pending_calls.values():
                            if not fut.done():
                                fut.set_exception(
                                    ConnectionError("[SODP] connection lost")
                                )
                        self._pending_calls.clear()
                        self._call_queue.clear()
                        if self._on_disconnect_cb:
                            self._on_disconnect_cb()

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.debug("[SODP] connection error: %s", e)

            if self._closed or not self._reconnect:
                break

            delay = min(
                self._reconnect_delay * (2 ** self._reconnect_attempts)
                + random.random() * 0.5,
                self._max_reconnect_delay,
            )
            self._reconnect_attempts += 1
            logger.debug(
                "[SODP] reconnecting in %.1fs (attempt %d)",
                delay, self._reconnect_attempts,
            )
            try:
                await asyncio.sleep(delay)
            except asyncio.CancelledError:
                break

    async def _sender(self, ws: websockets.WebSocketClientProtocol) -> None:
        while True:
            data = await self._send_queue.get()
            try:
                await ws.send(data)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.debug("[SODP] sender error: %s", e)
                break

    async def _recv_loop(self, ws: websockets.WebSocketClientProtocol) -> None:
        async for raw in ws:
            if not isinstance(raw, (bytes, bytearray)):
                continue
            try:
                frame = msgpack.unpackb(raw, raw=False)
                await self._handle_frame(frame)
            except Exception as e:
                logger.warning("[SODP] frame error: %s", e)

    # ── Frame handling ────────────────────────────────────────────────────────

    async def _handle_frame(self, frame: list) -> None:
        ftype, stream_id, _seq, body = frame
        if   ftype == _HELLO:      await self._on_hello(body)
        elif ftype == _AUTH_OK:    await self._on_auth_ok(body)
        elif ftype == _STATE_INIT: await self._on_state_init(stream_id, body)
        elif ftype == _DELTA:      await self._on_delta(stream_id, body)
        elif ftype == _RESULT:     self._on_result(body)
        elif ftype == _ERROR:      self._on_error(body)
        elif ftype == _HEARTBEAT:  self._send_raw(_HEARTBEAT, 0, None)

    async def _on_hello(self, body: dict) -> None:
        if body.get("auth"):
            token: str | None = None
            if self._token_provider is not None:
                result = self._token_provider()
                token = (await result) if asyncio.iscoroutine(result) else result  # type: ignore[assignment]
            else:
                token = self._token
            if not token:
                logger.error("[SODP] server requires auth but no token or token_provider supplied")
                if self._ws:
                    await self._ws.close()
                return
            self._send_raw(_AUTH, 0, {"token": token})
        else:
            await self._on_ready()

    async def _on_auth_ok(self, body: dict) -> None:
        logger.debug('[SODP] authenticated as "%s"', body.get("sub"))
        await self._on_ready()

    async def _on_ready(self) -> None:
        self._authenticated = True

        # Re-subscribe: use RESUME for keys we have a version for (missed deltas),
        # WATCH for brand-new or never-initialised keys.
        for key, entry in self._watches.items():
            if not entry.callbacks:
                continue
            if entry.version > 0:
                self._send_resume(key, entry.version)
            else:
                self._send_watch(key)

        # Flush calls queued before authentication.
        for ftype, stream_id, body in self._call_queue:
            self._send_raw(ftype, stream_id, body)
        self._call_queue.clear()

        self._ready_event.set()
        if self._on_connect_cb:
            self._on_connect_cb()

    async def _on_state_init(self, stream_id: int, body: dict) -> None:
        key   = body.get("state", "")
        entry = self._watches.get(key)
        if not entry:
            return

        self._stream_to_key[stream_id] = key
        entry.version     = body["version"]
        entry.value       = body["value"]
        entry.initialized = body["initialized"]

        meta = WatchMeta(version=entry.version, initialized=entry.initialized)
        await self._fire_callbacks(entry, entry.value, meta)

    async def _on_delta(self, stream_id: int, body: dict) -> None:
        key   = self._stream_to_key.get(stream_id)
        entry = self._watches.get(key) if key else None
        if not entry:
            return

        ops = body.get("ops", [])
        entry.value   = apply_ops(entry.value, ops)
        entry.version = body["version"]

        is_deleted = any(
            op.get("op") == "REMOVE" and op.get("path") == "/"
            for op in ops
        )
        entry.initialized = not is_deleted

        meta = WatchMeta(version=entry.version, initialized=entry.initialized)
        await self._fire_callbacks(entry, entry.value, meta)

    def _on_result(self, body: dict) -> None:
        call_id = body.get("call_id", "")
        fut = self._pending_calls.pop(call_id, None)
        if not fut or fut.done():
            return
        if body.get("success"):
            fut.set_result(body.get("data"))
        else:
            fut.set_exception(
                RuntimeError(f"[SODP] call failed: {body.get('data')}")
            )

    def _on_error(self, body: dict) -> None:
        msg = f"[SODP] ERROR {body.get('code')}: {body.get('message')}"
        logger.warning(msg)
        # ERROR frames don't carry a call_id — reject the oldest pending call.
        if self._pending_calls:
            call_id, fut = next(iter(self._pending_calls.items()))
            del self._pending_calls[call_id]
            if not fut.done():
                fut.set_exception(RuntimeError(msg))

    async def _fire_callbacks(
        self, entry: _WatchEntry, value: Any, meta: WatchMeta
    ) -> None:
        for cb in list(entry.callbacks):
            result = cb(value, meta)
            if asyncio.iscoroutine(result):
                await result

    # ── Sending helpers ───────────────────────────────────────────────────────

    def _send_raw(self, ftype: int, stream_id: int, body: Any) -> None:
        self._next_seq += 1
        data = msgpack.packb(
            [ftype, stream_id, self._next_seq, body], use_bin_type=True
        )
        self._send_queue.put_nowait(data)

    def _send_or_queue(self, ftype: int, stream_id: int, body: Any) -> None:
        if self._authenticated:
            self._send_raw(ftype, stream_id, body)
        else:
            self._call_queue.append((ftype, stream_id, body))

    def _send_watch(self, key: str) -> None:
        self._send_raw(_WATCH, self._next_stream_id, {"state": key})
        self._next_stream_id += 1

    def _send_resume(self, key: str, since_version: int) -> None:
        self._send_raw(
            _RESUME, self._next_stream_id,
            {"state": key, "since_version": since_version},
        )
        self._next_stream_id += 1

    # ── Public API ────────────────────────────────────────────────────────────

    def watch(self, key: str, callback: WatchCallback) -> Callable[[], None]:
        """
        Subscribe to a state key.

        *callback* is called with ``(value, meta)`` on every update and
        immediately with the cached value if the key is already known.
        It may be a plain function or an ``async`` function.

        Returns an unsubscribe function that removes only this callback
        (the server subscription stays alive until ``unwatch()``).
        """
        entry = self._watches.get(key)
        if not entry:
            entry = _WatchEntry(key=key)
            self._watches[key] = entry
            if self._authenticated:
                self._send_watch(key)
        elif entry.initialized:
            # Fire immediately with the cached snapshot.
            meta   = WatchMeta(version=entry.version, initialized=True)
            result = callback(entry.value, meta)
            if asyncio.iscoroutine(result):
                asyncio.create_task(result)

        entry.callbacks.add(callback)

        def unsub() -> None:
            entry.callbacks.discard(callback)

        return unsub

    def unwatch(self, key: str) -> None:
        """
        Cancel the server subscription for *key* and clear all local state.

        Different from the per-callback unsubscribe function returned by
        ``watch()``: this removes the key entirely and tells the server to
        stop sending DELTAs for it.
        """
        if key not in self._watches:
            return
        if self._authenticated:
            self._send_raw(_UNWATCH, 0, {"state": key})
        del self._watches[key]
        self._stream_to_key = {
            sid: k for sid, k in self._stream_to_key.items() if k != key
        }

    def get(self, key: str) -> Any:
        """
        Current cached value for *key*.

        Returns ``None`` when watched but not yet initialised, or when the
        key has no value on the server.  Returns ``None`` (not ``undefined``
        as in JS) when the key is not being watched — use ``is_watching``
        to distinguish the two cases.
        """
        entry = self._watches.get(key)
        return entry.value if entry else None

    def is_watching(self, key: str) -> bool:
        """Return ``True`` if this client has an active subscription for *key*."""
        return key in self._watches

    async def call(self, method: str, args: dict) -> Any:
        """
        Invoke a server method and await the RESULT.

        Built-in methods:
        - ``"state.set"``    — replace the full value of a state key
        - ``"state.patch"``  — deep-merge a partial object into existing state
        - ``"state.set_in"`` — atomically set a nested field by JSON-pointer
        - ``"state.delete"`` — remove a key entirely
        """
        call_id = str(uuid.uuid4())
        loop    = asyncio.get_running_loop()
        fut: asyncio.Future = loop.create_future()
        self._pending_calls[call_id] = fut
        self._send_or_queue(_CALL, 0, {"call_id": call_id, "method": method, "args": args})
        try:
            return await asyncio.wait_for(fut, timeout=30.0)
        except asyncio.CancelledError:
            self._pending_calls.pop(call_id, None)
            raise
        except asyncio.TimeoutError:
            self._pending_calls.pop(call_id, None)
            raise TimeoutError(f"[SODP] call timeout: {method}")

    async def set(self, key: str, value: Any) -> Any:
        """Replace the full value of *key*."""
        return await self.call("state.set", {"state": key, "value": value})

    async def patch(self, key: str, partial: dict) -> Any:
        """Deep-merge *partial* into the existing value of *key*."""
        return await self.call("state.patch", {"state": key, "patch": partial})

    async def presence(self, key: str, path: str, value: Any) -> Any:
        """
        Set a nested *path* within *key* and bind it to this session's lifetime.

        When the client disconnects for any reason the server auto-removes the path
        and broadcasts the DELTA — eliminating ghost presence entries.

        Example::

            await client.presence("collab.cursors", "/alice", {"name": "Alice", "line": 1})
        """
        return await self.call("state.presence", {"state": key, "path": path, "value": value})

    def state(self, key: str) -> "StateRef":
        """Return a typed handle scoped to *key*."""
        return StateRef(self, key)


# ── StateRef ───────────────────────────────────────────────────────────────────

class StateRef:
    """
    A key-scoped handle for reading and writing a single SODP state key.
    Obtain via ``client.state("my.key")``.

    Example::

        player = client.state("game.player")

        unsub = player.watch(lambda v, m: print(v))
        await player.set({"name": "Alice", "health": 100})
        await player.patch({"health": 80})
        await player.set_in("/position/x", 5)
        await player.delete()
    """

    def __init__(self, client: SodpClient, key: str) -> None:
        self._client = client
        self.key     = key

    def watch(self, callback: WatchCallback) -> Callable[[], None]:
        """Subscribe to this key.  Returns an unsubscribe function."""
        return self._client.watch(self.key, callback)

    def get(self) -> Any:
        """Current cached value."""
        return self._client.get(self.key)

    def is_watching(self) -> bool:
        """Return ``True`` if this key is currently subscribed."""
        return self._client.is_watching(self.key)

    def unwatch(self) -> None:
        """Cancel the server subscription and clear all local state for this key."""
        self._client.unwatch(self.key)

    async def set(self, value: Any) -> Any:
        """Replace the full value of this key."""
        return await self._client.set(self.key, value)

    async def patch(self, partial: dict) -> Any:
        """Deep-merge *partial* into existing state."""
        return await self._client.patch(self.key, partial)

    async def set_in(self, path: str, value: Any) -> Any:
        """Atomically set a nested field by JSON-pointer path."""
        return await self._client.call(
            "state.set_in", {"state": self.key, "path": path, "value": value}
        )

    async def delete(self) -> Any:
        """Remove this key from the server entirely."""
        return await self._client.call("state.delete", {"state": self.key})

    async def presence(self, path: str, value: Any) -> Any:
        """Bind a nested *path* to this session's lifetime (auto-removed on disconnect)."""
        return await self._client.presence(self.key, path, value)
