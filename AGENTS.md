# SODP Agent Guide

Reference for AI agents generating code that uses the State-Oriented Data Protocol.
Covers the protocol model, all four client SDKs, the Go embeddable server, and
common patterns. Read this before writing any SODP code.

---

## Protocol model

SODP is a continuous state-synchronization protocol over WebSocket + MessagePack.

```
client → WATCH key        # subscribe to a state key
server → STATE_INIT value  # full snapshot (initialized=false if key never written)
server → DELTA ops         # only the changed fields on each mutation
client → CALL set key val  # write state; triggers DELTA to all watchers
```

**State keys** are dot-separated strings: `"game.score"`, `"room.players"`,
`"collab.cursors"`. A key holds any JSON value (object, array, scalar).

**Delta semantics** — one CALL that changes 3 of 100 fields sends a 3-field delta,
not the full object. Clients apply deltas to their local snapshot automatically.

**WatchMeta** is delivered with every callback. Fields identical across all SDKs:

| Field | Type | Meaning |
|-------|------|---------|
| `version` | integer | Monotonically increasing mutation counter for this key |
| `initialized` | boolean | False if the key has never been written on the server |
| `source` | string | `"cache"` (replay from local store), `"init"` (STATE_INIT from server), `"delta"` (incremental update) |

Use `source === "init"` / `source == "init"` to distinguish the initial snapshot from
subsequent changes. Use `initialized` to detect a key that does not exist yet.

---

## TypeScript — `@sodp/client`

Install: `npm install @sodp/client`

### Construction

```typescript
import { SodpClient } from "@sodp/client";

// Anonymous (no auth)
const client = new SodpClient("ws://localhost:7777");

// Static JWT
const client = new SodpClient("ws://localhost:7777", { token: myJwt });

// Dynamic token (called on every connect/reconnect)
const client = new SodpClient("ws://localhost:7777", {
  tokenProvider: () => fetchFreshToken(),
});

// Node.js < 21 — pass the ws package
import WebSocket from "ws";
const client = new SodpClient("ws://localhost:7777", { WebSocket });

await client.ready; // wait until connected and authenticated
```

### Watch

```typescript
// Returns an unsubscribe function
const unsub = client.watch<PlayerState>("game.player", (value, meta) => {
  if (!meta.initialized) return;          // key not written yet
  if (meta.source === "init") console.log("initial snapshot:", value);
  else console.log("delta:", value);
});

// Watch multiple keys with one callback
const unsub = client.watchMany(
  ["game.score", "game.timer"],
  (value, meta) => { /* called per-key */ }
);

unsub(); // stop receiving updates
```

### Write

```typescript
// Replace the full value
await client.set("game.score", { value: 42, player: "alice" });

// Shallow merge — only named fields change
await client.patch("game.score", { value: 99 });

// Set a nested field by JSON Pointer
await client.state("game.player").setIn("/position/x", 10);

// Delete a key
await client.state("game.player").delete();

// Presence — auto-removed when this client disconnects
await client.presence("collab.cursors", "/user_abc123", { line: 42 });
```

### StateRef

```typescript
const player = client.state<PlayerState>("game.player");

const unsub = player.watch((value, meta) => console.log(value));
await player.set({ name: "Alice", health: 100 });
await player.patch({ health: 80 });
await player.setIn("/position/x", 5);
await player.delete();
await player.presence("/alice", { line: 3 });

const snapshot = player.getSnapshot(); // T | null — last known value, no async
```

### RPC

```typescript
// CALL dispatched to a handler registered on the server
const result = await client.call("compute.sum", { a: 1, b: 2 });
```

---

## React — `@sodp/react`

Install: `npm install @sodp/react @sodp/client`

### Provider

```tsx
import { SODPProvider } from "@sodp/react";

// Wrap your app — creates one shared SodpClient
<SODPProvider url="ws://localhost:7777" token={jwt}>
  <App />
</SODPProvider>
```

### Hooks

```tsx
import { useSodpState, useSodpStates, useSodpRef } from "@sodp/react";

// Single key — re-renders on every update
function Score() {
  const [score, meta] = useSodpState<number>("game.score");
  if (!meta.initialized) return <span>Loading…</span>;
  return <span>{score}</span>;
}

// Multiple keys in one subscription
function Dashboard() {
  const states = useSodpStates<number>(["game.score", "game.timer"]);
  const score = states.get("game.score")?.[0];
  const timer = states.get("game.timer")?.[0];
  return <div>{score} / {timer}</div>;
}

// StateRef — write without subscribing
function Controls() {
  const ref = useSodpRef("game.player");
  return (
    <button onClick={() => ref.patch({ health: 0 })}>Kill</button>
  );
}
```

### Access the raw client

```tsx
import { useSodpClient, useSodpConnected } from "@sodp/react";

function Status() {
  const client    = useSodpClient();
  const connected = useSodpConnected();
  return <span>{connected ? "online" : "offline"}</span>;
}
```

---

## Python — `sodp`

Install: `pip install sodp`

### Construction

```python
from sodp import SodpClient

# Anonymous
client = SodpClient("ws://localhost:7777")

# Static JWT
client = SodpClient("ws://localhost:7777", token=jwt)

# Dynamic token — sync or async callable
client = SodpClient("ws://localhost:7777", token_provider=get_fresh_token)

# Lifecycle callbacks
client = SodpClient(
    "ws://localhost:7777",
    on_connect=lambda: print("connected"),
    on_disconnect=lambda: print("disconnected"),
)

await client.ready  # or: await client (same thing)
```

### Watch

```python
# Returns an unsubscribe callable
unsub = client.watch("game.player", lambda value, meta: print(value))

# With params (echoed in STATE_INIT)
unsub = client.watch("game.player", callback, params={"since": 100})

# Multiple keys
unsub = client.watch_many(["game.score", "game.timer"], callback)

unsub()  # unsubscribe
```

### Write

```python
await client.set("game.player", {"name": "Alice", "health": 100})
await client.patch("game.player", {"health": 80})
await client.call("compute.sum", {"a": 1, "b": 2})
await client.presence("collab.cursors", "/user_abc", {"line": 42})

client.close()
```

### StateRef

```python
player = client.state("game.player")  # returns StateRef

unsub = player.watch(lambda value, meta: print(value))
await player.set({"name": "Alice", "health": 100})
await player.patch({"health": 80})
await player.set_in("/position/x", 10)   # JSON Pointer path
await player.delete()
await player.presence("/alice", {"line": 3})

snapshot = player.get()           # last known value, synchronous
watching = player.is_watching()   # bool
```

### Full example

```python
import asyncio
from sodp import SodpClient

async def main():
    client = SodpClient("ws://localhost:7777")
    await client.ready

    player = client.state("game.player")
    events = []
    player.watch(lambda v, m: events.append(v))

    await player.set({"name": "Alice", "health": 100})
    await asyncio.sleep(0.1)   # let the delta arrive
    print(events)              # [{"name": "Alice", "health": 100}]

    client.close()

asyncio.run(main())
```

---

## Java — `site.orkestri:sodp-client`

```xml
<dependency>
  <groupId>site.orkestri</groupId>
  <artifactId>sodp-client</artifactId>
  <version>0.2.2</version>
</dependency>
```

### Construction

```java
SodpClient client = SodpClient.builder("ws://localhost:7777")
    .token("eyJ...")                // optional static JWT
    .tokenProvider(() -> getJwt())  // dynamic token — called on each connect
    .onConnect(() -> log.info("connected"))
    .onDisconnect(() -> log.info("disconnected"))
    .objectMapper(sharedMapper)     // share with framework (optional)
    .build();

client.ready().get();  // block until authenticated
```

### Watch

```java
// Raw JsonNode
Runnable unsub = client.watch("game.player", (value, meta) -> {
    if (!meta.initialized()) return;
    System.out.println(value);
});

// Typed deserialization
Runnable unsub = client.watch("game.player", PlayerState.class, (player, meta) -> {
    System.out.println(player.getName());
});

// With params
Runnable unsub = client.watch("game.player", PlayerState.class, callback,
    Map.of("since", 100));

// Multiple keys
Runnable unsub = client.watchMany(
    List.of("game.score", "game.timer"), Integer.class, callback);

unsub.run();  // unsubscribe
```

### Write

```java
client.set("game.player", new PlayerState("Alice", 100)).get();
client.patch("game.player", Map.of("health", 80)).get();
client.setIn("game.player", "/position/x", 10).get();
client.delete("game.player").get();
client.presence("collab.cursors", "/user_abc", Map.of("line", 42)).get();
client.call("compute.sum", Map.of("a", 1, "b", 2)).get();

client.close();
```

### StateRef

```java
StateRef<PlayerState> player = client.state("game.player", PlayerState.class);

Runnable unsub = player.watch((value, meta) -> System.out.println(value));
player.set(new PlayerState("Alice", 100)).get();
player.patch(Map.of("health", 80)).get();
player.setIn("/position/x", 10).get();
player.delete().get();
player.presence("/alice", Map.of("line", 3)).get();

Optional<PlayerState> snapshot = player.getSnapshot();
boolean watching = client.isWatching("game.player");
```

---

## Go — embeddable server (`sodp-go`)

`sodp-go` is a **server library**, not a client. Use it to embed a SODP server
inside a Go HTTP service. There is no Go SODP client yet — use the TypeScript,
Python, or Java clients to connect from other processes.

```go
import (
    sodp "github.com/orkestri/sodp-go"
    "net/http"
)
```

### Start a server

```go
srv := sodp.NewServer(
    sodp.WithJWTSecret([]byte("my-secret")),   // HS256 auth
    sodp.WithACLFile("config/acl.json"),
    sodp.WithPersistenceDir("/var/lib/sodp"),
    sodp.WithMaxSessions(10_000),
    sodp.WithBackpressureLimit(1024),
)
defer srv.Close()

http.HandleFunc("/sodp", srv.HandleWS)
http.ListenAndServe(":7777", nil)
```

### Write state from server-side code

```go
// Full replacement — triggers DELTA to all watchers
srv.Mutate("game.score", map[string]any{"value": 42, "player": "alice"})

// Append element to an array key, cap at maxLen
srv.MutateAppend("game.events", event, 1000)

// Delete a key
srv.MutateDelete("game.player")
```

### Server options

| Option | Purpose |
|--------|---------|
| `WithJWTSecret([]byte)` | HS256 token validation |
| `WithJWTPublicKey(pem string)` | RS256 token validation |
| `WithACLFile(path)` | JSON ACL rules for per-key read/write permissions |
| `WithPersistenceDir(dir)` | Replay log directory for RESUME support |
| `WithMaxSessions(n)` | Reject connections above this limit |
| `WithBackpressureLimit(n)` | Outbound channel capacity per session (default 1024) |
| `WithRateLimit(n)` | Max writes per second per session |
| `WithMaxWatches(n)` | Max concurrent subscriptions per session |
| `WithCluster(backend)` | Horizontal scaling via `ClusterBackend` interface |
| `WithAllowedOrigins([]string)` | WebSocket origin check |
| `WithAuthorizeKey(fn)` | Custom per-key authorization function |

---

## JWT authentication

All SDKs support HS256 and RS256. The server verifies the token and makes
`sub` + extra claims available for ACL matching.

**Server side (Rust)**

```bash
# HS256
SODP_JWT_SECRET=my-secret ./sodp-server 0.0.0.0:7777

# RS256
SODP_JWT_PUBLIC_KEY_FILE=/etc/sodp/public.pem ./sodp-server 0.0.0.0:7777
```

**Client side (TypeScript)**

```typescript
// Static — pass once
new SodpClient(url, { token: jwt });

// Dynamic — called on every connect/reconnect so tokens never expire mid-session
new SodpClient(url, { tokenProvider: () => fetchFreshJwt() });
```

---

## ACL file format

```json
[
  { "pattern": "private.{sub}", "read": ["{sub}"], "write": ["{sub}"] },
  { "pattern": "admin.*",       "read": ["role:admin"], "write": ["role:admin"] },
  { "pattern": "public.*",      "read": ["*"],          "write": ["role:writer"] }
]
```

- `{sub}` — captures the JWT `sub` claim
- `*` — suffix wildcard (matches one or more remaining segments)
- Permission values: `"*"` (everyone), `"{sub}"` (own sub only),
  `"role:X"`, `"group:X"`, `"perm:X"`, literal string

---

## Common patterns

### Presence (live cursors, active users)

Presence entries are automatically removed when the session disconnects —
no ghost entries, no cleanup required.

```typescript
// TypeScript
await client.presence("collab.cursors", `/user_${userId}`, { line: 42 });

const unsub = client.watch<CursorMap>("collab.cursors", (cursors) => {
  renderCursors(Object.entries(cursors ?? {}));
});
```

```python
# Python
await client.presence("collab.cursors", f"/user_{user_id}", {"line": 42})
```

### Detecting an uninitialized key

```typescript
client.watch("game.player", (value, meta) => {
  if (!meta.initialized) {
    // Key has never been written — initialize it
    client.set("game.player", defaultPlayer);
    return;
  }
  // Normal update path
});
```

### Read once (no long-lived subscription)

```typescript
// Subscribe, wait for the first callback, then unsubscribe
const value = await new Promise(resolve => {
  const unsub = client.watch("game.score", (v) => { unsub(); resolve(v); });
});
```

```python
# Python
event = asyncio.Event()
result = {}

def cb(value, meta):
    result["value"] = value
    event.set()

unsub = client.watch("game.score", cb)
await event.wait()
unsub()
```

### Offline detection (React)

```tsx
function ConnectionBanner() {
  const connected = useSodpConnected();
  if (connected) return null;
  return <div className="banner">Reconnecting…</div>;
}
```

### Fanout across services

A mutation from any client or server-side `Mutate()` call fans out to all
watchers of that key across all connected sessions — including other servers
in a Redis cluster.

```go
// Go server: push a game event to all browser clients watching "game.events"
srv.MutateAppend("game.events", GameEvent{Type: "goal", Team: "red"}, 500)
```

---

## Key names and namespacing

Use dot-separated segments to organize state:

```
game.{roomId}.score      per-room scores
game.{roomId}.players    per-room player list
user.{userId}.profile    per-user data
collab.{docId}.cursors   per-document presence
```

No wildcards in key names — each `watch(key)` is an exact match.

---

## What SODP is NOT

- Not a pub/sub message broker — state, not events. The server holds the current
  value; late joiners always get the latest snapshot, not replayed history.
- Not a database — no query language, no secondary indexes.
- Not REST or GraphQL — no request/response per field; subscribe once, receive
  only diffs.
- Not gRPC streaming — lighter weight, browser-native (WebSocket).
