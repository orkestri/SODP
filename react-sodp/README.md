# @sodp/react

[![npm](https://img.shields.io/npm/v/@sodp/react)](https://www.npmjs.com/package/@sodp/react)
[![license](https://img.shields.io/github/license/orkestri/SODP)](https://github.com/orkestri/SODP/blob/main/LICENSE)

React bindings for the **State-Oriented Data Protocol (SODP)** — real-time state sync in your components with a single hook.

Every component that calls `useSodpState("my.key")` automatically re-renders when the server-side value changes. Multiple components watching the same key share a single WebSocket subscription.

→ [Protocol spec & server](https://github.com/orkestri/SODP) · [Core client (@sodp/client)](https://www.npmjs.com/package/@sodp/client)

---

## Install

```bash
npm install @sodp/react @sodp/client
# or
yarn add @sodp/react @sodp/client
```

Requires **React 18+**.

---

## Quick start

Wrap your app in `SODPProvider` and call `useSodpState` anywhere inside:

```tsx
// main.tsx
import { SODPProvider } from "@sodp/react";

root.render(
  <SODPProvider url="wss://sodp.example.com" token={jwt}>
    <App />
  </SODPProvider>
);
```

```tsx
// HUD.tsx
import { useSodpState } from "@sodp/react";

interface Player {
  name: string;
  health: number;
  position: { x: number; y: number };
}

function HUD() {
  const [player, meta, ref] = useSodpState<Player>("game.player");

  if (!meta?.initialized) return <div>Loading…</div>;

  return (
    <div>
      <span>{player?.name} — HP: {player?.health}</span>
      <button onClick={() => ref?.patch({ health: (player?.health ?? 0) - 10 })}>
        Take damage
      </button>
    </div>
  );
}
```

The component re-renders automatically on every delta — no polling, no manual subscriptions.

---

## Provider

### `<SODPProvider>`

Place once near the root of your app. All hooks inside must be descendants of this provider.

```tsx
<SODPProvider
  url="wss://sodp.example.com"

  // Authentication — pick one:
  token={staticJwt}
  tokenProvider={async () => {
    const res = await fetch("/api/sodp-token");
    return res.text();
  }}

  // Reconnect settings (optional — defaults shown):
  reconnect={true}
  reconnectDelay={1000}
  maxReconnectDelay={30000}

  // Lifecycle callbacks (optional):
  onConnect={() => console.log("connected")}
  onDisconnect={() => console.log("disconnected")}
>
  <App />
</SODPProvider>
```

The client is created once on mount and closed on unmount. Changing `url` closes the old client and opens a new one. All other options are read on mount — to rotate a token, remount the provider.

---

## Hooks

### `useSodpState<T>(key)`

Subscribe to a state key and re-render on every update.

Returns a `[value, meta, ref]` tuple:

| Element | Type | Description |
|---|---|---|
| `value` | `T \| null` | Current state; `null` if the key has no value yet |
| `meta` | `WatchMeta \| null` | `{ version, initialized }`, or `null` before first update |
| `ref` | `StateRef<T> \| null` | Handle for writes; `null` while connecting |

```tsx
function ScoreBoard() {
  const [score, meta, ref] = useSodpState<{ value: number }>("game.score");

  return (
    <div>
      <strong>{score?.value ?? "—"}</strong>
      <button onClick={() => ref?.set({ value: 0 })}>Reset</button>
    </div>
  );
}
```

Multiple components calling `useSodpState` with the same key receive the same updates and share one server subscription. When the key changes, the previous key's value is cleared immediately so stale data is never shown.

---

### `useSodpRef<T>(key)`

Returns a `StateRef<T>` for **write-only** access — without subscribing to updates or causing re-renders on change.

Use this in buttons, form handlers, and other places that mutate state but don't need to display it:

```tsx
function ResetButton() {
  const ref = useSodpRef<{ score: number }>("game.score");

  return (
    <button onClick={() => ref?.set({ score: 0 })}>
      Reset score
    </button>
  );
}
```

---

### `useSodpConnected()`

Returns `true` once the client is connected and authenticated. Use it to show a connection status indicator:

```tsx
function StatusBar() {
  const connected = useSodpConnected();
  return (
    <span style={{ color: connected ? "green" : "orange" }}>
      {connected ? "Live" : "Reconnecting…"}
    </span>
  );
}
```

---

### `useSodpClient()`

Returns the raw `SodpClient` from the nearest provider. Useful when you need direct access to `call()`, `watch()`, or `presence()`:

```tsx
function CursorTracker() {
  const client = useSodpClient();

  useEffect(() => {
    if (!client) return;
    // Bind cursor to session lifetime — auto-removed on disconnect
    client.presence("collab.cursors", "/alice", { name: "Alice", line: 1 });
  }, [client]);

  return null;
}
```

Returns `null` during the brief window between mount and the first WebSocket connection.

---

## Presence (collaborative features)

Presence binds a nested path to the session lifetime. The server automatically removes it and notifies all watchers when the browser tab closes or the network drops — no stale cursors, no ghost "online" badges:

```tsx
function CollabEditor() {
  const client = useSodpClient();
  const [cursors] = useSodpState<Record<string, { name: string; line: number }>>(
    "collab.cursors"
  );

  useEffect(() => {
    if (!client) return;
    client.presence("collab.cursors", "/alice", { name: "Alice", line: 1 });
  }, [client]);

  return (
    <div>
      {Object.entries(cursors ?? {}).map(([id, c]) => (
        <div key={id}>{c.name} is on line {c.line}</div>
      ))}
    </div>
  );
}
```

---

## StateRef write API

`ref` values returned by `useSodpState` and `useSodpRef` expose the full write surface:

```ts
await ref.set(value)                  // replace full value
await ref.patch({ field: newValue })  // deep-merge partial update
await ref.setIn("/a/b/c", value)      // set nested field by JSON Pointer
await ref.delete()                    // remove key entirely
await ref.presence(path, value)       // session-lifetime path binding
ref.watch(callback)                   // subscribe (returns unsub fn)
ref.get()                             // read cached snapshot
ref.unwatch()                         // cancel subscription + clear local state
```

---

## TypeScript

All hooks and components are fully typed. Pass your state interface as the type parameter:

```tsx
interface GameState {
  round: number;
  players: Record<string, { name: string; score: number }>;
}

const [game, meta, ref] = useSodpState<GameState>("game.state");

// ref is StateRef<GameState> — set/patch/setIn are typed
await ref?.patch({ round: 2 });
```
