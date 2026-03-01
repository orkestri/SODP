# @sodp/client

[![npm](https://img.shields.io/npm/v/@sodp/client)](https://www.npmjs.com/package/@sodp/client)
[![license](https://img.shields.io/github/license/orkestri/SODP)](https://github.com/orkestri/SODP/blob/main/LICENSE)

TypeScript / JavaScript client for the **State-Oriented Data Protocol (SODP)** — a WebSocket-based protocol for continuous state synchronization.

Instead of polling or request/response, SODP streams every change as a minimal delta to all connected subscribers. One mutation to a 100-field object sends exactly the changed fields.

→ [Protocol spec & server](https://github.com/orkestri/SODP) · [React bindings (@sodp/react)](https://www.npmjs.com/package/@sodp/react)

---

## Install

```bash
npm install @sodp/client
# or
yarn add @sodp/client
```

### Node.js < 21

Node.js 21+ has a native `WebSocket`. For older versions, pass the `ws` package:

```bash
npm install @sodp/client ws
```

```ts
import WebSocket from "ws";
import { SodpClient } from "@sodp/client";

const client = new SodpClient("ws://localhost:7777", { WebSocket });
```

---

## Quick start

```ts
import { SodpClient } from "@sodp/client";

const client = new SodpClient("wss://sodp.example.com", { token: myJwt });

// Wait for the connection to be ready (optional — watch() queues automatically)
await client.ready;

// Subscribe to a state key
const unsub = client.watch<{ score: number }>("game.score", (value, meta) => {
  console.log("score:", value?.score, "version:", meta.version);
});

// Mutate state
await client.set("game.score", { score: 42 });

// Stop receiving updates for this callback
unsub();

// Close the connection entirely
client.close();
```

---

## Authentication

```ts
// Static token (simple)
const client = new SodpClient(url, { token: "eyJhbG..." });

// Dynamic token provider — called on every connect/reconnect.
// Use this to get a fresh token when the previous one expires.
const client = new SodpClient(url, {
  tokenProvider: async () => {
    const res = await fetch("/api/sodp-token");
    return res.text();
  },
});
```

---

## API reference

### `new SodpClient(url, options?)`

| Option | Type | Default | Description |
|---|---|---|---|
| `token` | `string` | — | Static JWT token |
| `tokenProvider` | `() => string \| Promise<string>` | — | Called on every connect; supersedes `token` |
| `WebSocket` | `typeof WebSocket` | native | Custom WebSocket constructor (Node.js < 21) |
| `reconnect` | `boolean` | `true` | Auto-reconnect on disconnect |
| `reconnectDelay` | `number` | `1000` | Base reconnect delay in ms (doubles per attempt) |
| `maxReconnectDelay` | `number` | `30000` | Maximum reconnect delay in ms |
| `onConnect` | `() => void` | — | Called each time the connection is established |
| `onDisconnect` | `() => void` | — | Called each time the connection drops |

---

### `client.ready: Promise<void>`

Resolves once the client is connected and authenticated. Subsequent reconnects reuse the same promise (it never re-pends). You rarely need to `await` it directly — `watch()` and `call()` queue automatically.

---

### `client.watch<T>(key, callback): () => void`

Subscribe to a state key. `callback(value, meta)` fires on every server-side change and immediately with the cached value if the key is already known.

- `value` — current state typed as `T`, or `null` if the key has no value yet
- `meta.version` — monotonically increasing version number
- `meta.initialized` — `false` when the key has never been written to the server

Returns an **unsubscribe function**. Multiple `watch()` calls for the same key share a single server subscription.

```ts
const unsub = client.watch<{ name: string; health: number }>("game.player", (player, meta) => {
  if (!meta.initialized) return; // key not yet written
  console.log(player?.name, player?.health);
});

// Later:
unsub(); // removes this callback; server subscription stays alive
```

---

### `client.state<T>(key): StateRef<T>`

Returns a typed handle scoped to a single key. Cleaner than passing the key to every method:

```ts
interface Player { name: string; health: number; position: { x: number; y: number } }

const player = client.state<Player>("game.player");

// Subscribe
const unsub = player.watch((value, meta) => console.log(value?.name));

// Write
await player.set({ name: "Alice", health: 100, position: { x: 0, y: 0 } });
await player.patch({ health: 80 });                  // only health changes
await player.setIn("/position/x", 5);               // atomic nested field update
await player.delete();                               // remove the key entirely

// Read snapshot
const current = player.get();

// Cancel everything
player.unwatch();
```

---

### `client.call(method, args): Promise<data>`

Invoke a built-in server method:

| Method | Args | Effect |
|---|---|---|
| `state.set` | `{ state, value }` | Replace the full value |
| `state.patch` | `{ state, patch }` | Deep-merge patch into existing value |
| `state.set_in` | `{ state, path, value }` | Set a nested field by JSON Pointer |
| `state.delete` | `{ state }` | Remove the key entirely |
| `state.presence` | `{ state, path, value }` | Set a path bound to session lifetime |

```ts
await client.call("state.set", { state: "game.score", value: { score: 0 } });
```

---

### Convenience methods

```ts
await client.set("game.score", { score: 42 });
await client.patch("game.player", { health: 80 });
await client.presence("collab.cursors", "/alice", { name: "Alice", line: 3 });
```

---

### Presence

Presence binds a nested path to the session lifetime. The server automatically removes it and notifies all watchers when the client disconnects — no ghost cursors or stale "online" flags:

```ts
// Register this session's cursor — auto-removed on tab close or network drop
await client.presence("collab.cursors", "/alice", { name: "Alice", line: 1, col: 5 });

// Or via StateRef:
const cursors = client.state("collab.cursors");
await cursors.presence("/alice", { name: "Alice", line: 1, col: 5 });
```

---

### `client.unwatch(key): void`

Cancel the server subscription and clear all local state for a key. Different from the per-callback unsubscribe returned by `watch()`: this removes all callbacks and tells the server to stop sending deltas.

---

### `client.getSnapshot<T>(key): T | null | undefined`

Synchronously read the cached value without subscribing. Returns `undefined` if the key is not being watched.

---

### `client.close(): void`

Gracefully close the WebSocket and stop reconnecting.

---

## Auto-reconnect & RESUME

The client reconnects automatically with exponential backoff (1 s → 2 s → 4 s → … → 30 s). After reconnecting:

- Keys with a known `version` send `RESUME { since_version }` — the server replays any missed deltas, then resumes live streaming. Your callbacks see every update in order.
- Keys with no version yet send `WATCH` — you receive the current snapshot via `STATE_INIT`.

No data is lost during short disconnections as long as the server's delta log for that key is not full (capacity: 1 000 deltas per key).

---

## StateRef API summary

| Method | Description |
|---|---|
| `ref.watch(cb)` | Subscribe; returns unsub function |
| `ref.get()` | Cached value (`T \| null \| undefined`) |
| `ref.isWatching()` | `true` if actively subscribed |
| `ref.unwatch()` | Cancel subscription + clear local state |
| `ref.set(value)` | Replace full value |
| `ref.patch(partial)` | Deep-merge partial |
| `ref.setIn(path, value)` | Set nested field by JSON Pointer |
| `ref.delete()` | Remove key from server |
| `ref.presence(path, value)` | Session-lifetime path binding |
