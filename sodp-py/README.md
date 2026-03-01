# sodp

[![PyPI](https://img.shields.io/pypi/v/sodp)](https://pypi.org/project/sodp/)
[![Python](https://img.shields.io/pypi/pyversions/sodp)](https://pypi.org/project/sodp/)
[![license](https://img.shields.io/github/license/orkestri/SODP)](https://github.com/orkestri/SODP/blob/main/LICENSE)

Python asyncio client for the **State-Oriented Data Protocol (SODP)** — a WebSocket-based protocol for continuous state synchronization.

Instead of polling or request/response, SODP streams every change as a minimal delta to all connected subscribers. One mutation to a 100-field object sends exactly the changed fields.

→ [Protocol spec & server](https://github.com/orkestri/SODP)

---

## Install

```bash
pip install sodp
```

Requires **Python 3.11+** and a running asyncio event loop.

---

## Quick start

```python
import asyncio
from sodp import SodpClient

async def main():
    client = SodpClient("ws://localhost:7777")
    await client.ready

    # Subscribe
    def on_player(value, meta):
        print(f"player: {value}  version={meta.version}")

    unsub = client.watch("game.player", on_player)

    # Mutate
    await client.set("game.player", {"name": "Alice", "health": 100})
    await client.patch("game.player", {"health": 80})   # only health changes

    await asyncio.sleep(1)

    unsub()        # remove this callback
    client.close() # close the connection

asyncio.run(main())
```

---

## Authentication

```python
# Static token
client = SodpClient("wss://sodp.example.com", token="eyJhbG...")

# Dynamic token provider — called on every connect/reconnect
async def get_token() -> str:
    async with aiohttp.ClientSession() as s:
        return await (await s.get("/api/sodp-token")).text()

client = SodpClient("wss://sodp.example.com", token_provider=get_token)

# Sync provider is also accepted
client = SodpClient(url, token_provider=lambda: os.environ["SODP_TOKEN"])
```

---

## API reference

### `SodpClient(url, *, ...)`

```python
client = SodpClient(
    url,                        # WebSocket URL, e.g. "ws://localhost:7777"
    token=None,                 # static JWT string
    token_provider=None,        # callable → str | Awaitable[str]; supersedes token
    reconnect=True,             # auto-reconnect on disconnect
    reconnect_delay=1.0,        # base reconnect delay in seconds (doubles per attempt)
    max_reconnect_delay=30.0,   # maximum reconnect delay in seconds
    on_connect=None,            # called each time the connection is established
    on_disconnect=None,         # called each time the connection drops
)
```

The client connects immediately in the background. Use `await client.ready` (or `await client`) to wait for the first successful authentication before sending commands.

---

### `await client.ready`

Awaitable that resolves once the client is connected and authenticated. You can also `await client` directly:

```python
await client.ready  # explicit
await client        # same thing
```

---

### `client.watch(key, callback) → unsub`

Subscribe to a state key. `callback(value, meta)` fires on every update and immediately with the cached value if the key is already known.

- `value` — current state (any JSON-compatible type), or `None` if the key has no value yet
- `meta.version` — monotonically increasing version number (`int`)
- `meta.initialized` — `False` when the key has never been written to the server

`callback` may be a plain function or an `async` function.

Returns an **unsubscribe callable**. Multiple `watch()` calls for the same key share a single server subscription.

```python
def on_player(value, meta):
    if not meta.initialized:
        return
    print(value["name"], value["health"])

unsub = client.watch("game.player", on_player)

# Async callback also works:
async def on_score(value, meta):
    await db.update_score(value)

unsub2 = client.watch("game.score", on_score)
```

---

### `client.state(key) → StateRef`

Returns a key-scoped handle for cleaner per-key code:

```python
player = client.state("game.player")

unsub = player.watch(lambda v, m: print(v))

await player.set({"name": "Alice", "health": 100, "position": {"x": 0, "y": 0}})
await player.patch({"health": 80})           # only health changes
await player.set_in("/position/x", 5)        # atomic nested field update
await player.delete()                        # remove the key entirely
await player.presence("/alice", {"line": 1}) # session-lifetime path

current = player.get()                       # cached snapshot
player.unwatch()                             # cancel subscription
```

---

### `await client.call(method, args) → data`

Invoke a built-in server method:

| Method | Args | Effect |
|---|---|---|
| `"state.set"` | `{"state": key, "value": v}` | Replace full value |
| `"state.patch"` | `{"state": key, "patch": {...}}` | Deep-merge partial update |
| `"state.set_in"` | `{"state": key, "path": "/a/b", "value": v}` | Set nested field by JSON Pointer |
| `"state.delete"` | `{"state": key}` | Remove key entirely |
| `"state.presence"` | `{"state": key, "path": "/p", "value": v}` | Session-lifetime path |

```python
await client.call("state.set", {"state": "game.score", "value": {"value": 0}})
```

---

### Convenience methods

```python
await client.set("game.score", {"value": 42})
await client.patch("game.player", {"health": 80})
await client.presence("collab.cursors", "/alice", {"name": "Alice", "line": 3})
```

---

### `client.unwatch(key)`

Cancel the server subscription and clear all local state for a key.

---

### `client.get(key) → Any`

Synchronously read the cached value without subscribing. Returns `None` if the key is not being watched or has no value.

---

### `client.is_watching(key) → bool`

Returns `True` if this client has an active subscription for `key`.

---

### `client.close()`

Gracefully close the connection and stop reconnecting.

---

## Presence

Presence binds a nested path to the session lifetime. The server automatically removes it and notifies all watchers when the client disconnects for any reason — no ghost cursors or stale "online" flags:

```python
# Bind cursor to this session — auto-removed if the process crashes or disconnects
await client.presence("collab.cursors", "/alice", {"name": "Alice", "line": 1})

# Or via StateRef:
cursors = client.state("collab.cursors")
await cursors.presence("/alice", {"name": "Alice", "line": 1})
```

---

## Auto-reconnect & RESUME

The client reconnects with exponential backoff (1 s → 2 s → 4 s → … → 30 s). After reconnecting:

- Keys with a known version send `RESUME` — the server replays missed deltas, then resumes live streaming
- Keys never seen yet send `WATCH` — you receive the current snapshot

No data is lost during short disconnections as long as the server's delta log is not full (1 000 deltas per key).

---

## StateRef API summary

```python
ref = client.state("my.key")

ref.watch(callback)          # subscribe; returns unsub callable
ref.get()                    # cached value
ref.is_watching()            # True if subscribed
ref.unwatch()                # cancel subscription + clear local state
await ref.set(value)         # replace full value
await ref.patch(partial)     # deep-merge partial
await ref.set_in(path, val)  # set nested field by JSON Pointer
await ref.delete()           # remove key from server
await ref.presence(path, v)  # session-lifetime path binding
```

---

## FastAPI example

```python
from contextlib import asynccontextmanager
from fastapi import FastAPI
from sodp import SodpClient

client: SodpClient

@asynccontextmanager
async def lifespan(app: FastAPI):
    global client
    client = SodpClient("ws://sodp-server:7777", token=os.environ["SODP_TOKEN"])
    await client.ready
    yield
    client.close()

app = FastAPI(lifespan=lifespan)

@app.post("/score/{value}")
async def set_score(value: int):
    await client.set("game.score", {"value": value})
    return {"ok": True}
```
