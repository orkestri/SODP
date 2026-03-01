# SODP Developer Guide

## Table of contents

1. [Core concepts](#1-core-concepts)
2. [State keys](#2-state-keys)
3. [Running the server](#3-running-the-server)
4. [TypeScript client](#4-typescript-client)
5. [Python client](#5-python-client)
6. [State methods reference](#6-state-methods-reference)
7. [Presence (session-scoped paths)](#7-presence)
8. [Authentication (JWT)](#8-authentication-jwt)
9. [Access control (ACL)](#9-access-control-acl)
10. [Rate limiting](#10-rate-limiting)
11. [Schema validation](#11-schema-validation)
12. [Reconnect and RESUME](#12-reconnect-and-resume)
13. [Health check and metrics](#13-health-check-and-metrics)
14. [Environment variable reference](#14-environment-variable-reference)

---

## 1. Core concepts

### The model

SODP replaces request-response with a subscription model. A client **WATCH**es a named key; the server responds with a full snapshot (**STATE_INIT**), then streams only the structural differences (**DELTA**) as other clients mutate the key.

```
client                          server
  │── WATCH "user.profile" ──▶ │
  │◀── STATE_INIT ─────────────│  { name: "Alice", score: 0 }
  │                            │
  │                     ── CALL "state.patch" ▶ { score: 1 }  (another client)
  │◀── DELTA ──────────────────│  [UPDATE /score → 1]
  │◀── DELTA ──────────────────│  [UPDATE /score → 2]
```

Every client maintains a local **replica** of each watched key. The replica is updated by applying delta operations locally — no refetch, no polling.

### Delta operations

Deltas are arrays of JSON-pointer operations:

| Operation | Meaning |
|---|---|
| `ADD` | New field or array element |
| `UPDATE` | Field value changed |
| `REMOVE` | Field or key deleted |

### Versions

Every mutation increments the key's monotonic **version** counter. Clients store the last-seen version; on reconnect they send a `RESUME` frame so the server can replay missed deltas since that version.

---

## 2. State keys

State keys are dot-separated strings. There is no enforced hierarchy — the dots are just naming convention.

```
game.score
user.alice.profile
collab.cursors
tenant.acme.settings
```

**Key rules:**
- Any UTF-8 string is valid.
- The server stores one independent state value per key.
- Keys are created on first write and deleted via `state.delete`.

---

## 3. Running the server

```bash
sodp-server <addr> [<log-dir>] [<schema-file>]
```

| Argument | Description |
|---|---|
| `addr` | Bind address, e.g. `0.0.0.0:7777` |
| `log-dir` | Optional. Directory for the segmented persistence log. Omit (or pass `""`) for in-memory only. |
| `schema-file` | Optional. Path to a JSON SDL file for server-side type validation. |

Examples:

```bash
# In-memory only
sodp-server 0.0.0.0:7777

# Persistent, no schema
sodp-server 0.0.0.0:7777 /var/lib/sodp/log

# Persistent + schema
sodp-server 0.0.0.0:7777 /var/lib/sodp/log /etc/sodp/schema.json

# Schema only, no persistence
sodp-server 0.0.0.0:7777 "" /etc/sodp/schema.json
```

All behavioural configuration is done via **environment variables** — see [§14](#14-environment-variable-reference).

---

## 4. TypeScript client

### Installation

```bash
npm install @sodp/client
```

Or build from source:

```bash
cd client-ts && npm install && npm run build
```

### Connecting

```typescript
import { SodpClient } from "@sodp/client";

// Unauthenticated
const client = new SodpClient("ws://localhost:7777");
await client.ready;

// With a static JWT
const client = new SodpClient("wss://sodp.example.com", { token: myJwt });
await client.ready;

// With a token provider (called on every connect/reconnect)
const client = new SodpClient("wss://sodp.example.com", {
  tokenProvider: async () => {
    const res = await fetch("/api/sodp-token");
    return res.text();
  },
});
await client.ready;

// Node.js < 21 (no native WebSocket)
import WebSocket from "ws";
const client = new SodpClient("ws://localhost:7777", { WebSocket });
await client.ready;
```

### Constructor options

| Option | Type | Default | Description |
|---|---|---|---|
| `token` | `string` | — | Static JWT. Used when `tokenProvider` is absent. |
| `tokenProvider` | `() => string \| Promise<string>` | — | Called on every connect. Supersedes `token`. |
| `WebSocket` | `typeof WebSocket` | `globalThis.WebSocket` | Custom WebSocket class (Node.js < 21). |
| `reconnect` | `boolean` | `true` | Auto-reconnect on drop. |
| `reconnectDelay` | `number` | `1000` | Base reconnect delay in ms (exponential backoff). |
| `maxReconnectDelay` | `number` | `30000` | Maximum reconnect delay in ms. |
| `onConnect` | `() => void` | — | Called each time the connection is established. |
| `onDisconnect` | `() => void` | — | Called each time the connection drops. |

### Subscribing

```typescript
// Returns an unsubscribe function
const unsub = client.watch<{ score: number }>("game.score", (value, meta) => {
  console.log(value?.score, meta.version, meta.initialized);
});

// Later:
unsub();  // removes this callback; the server subscription stays alive
client.unwatch("game.score");  // cancels the server subscription entirely
```

`WatchMeta` fields:
- `version: number` — monotonic server version when this value was written
- `initialized: boolean` — `false` when the key has never been written to the server

The callback fires:
- **Immediately** with the cached value if the key is already known locally.
- **On every delta** received from the server.

### StateRef — key-scoped handle

```typescript
interface Player { name: string; health: number; position: { x: number; y: number } }

const player = client.state<Player>("game.player");

const unsub = player.watch(value => console.log(value?.name));
await player.set({ name: "Alice", health: 100, position: { x: 0, y: 0 } });
await player.patch({ health: 80 });
await player.setIn("/position/x", 5);
await player.delete();
```

### Closing

```typescript
client.close();  // stops reconnecting, closes the WebSocket
```

---

## 5. Python client

### Installation

```bash
pip install sodp-py   # once published
# or from source:
pip install -e sodp-py/
```

### Connecting

```python
from sodp.client import SodpClient
import asyncio

async def main():
    # Unauthenticated
    client = SodpClient("ws://localhost:7777")
    await client.ready

    # Static JWT
    client = SodpClient("ws://localhost:7777", token=my_jwt)
    await client.ready

    # Token provider (sync or async callable)
    async def get_token():
        async with aiohttp.ClientSession() as s:
            return await (await s.get("/api/sodp-token")).text()

    client = SodpClient("ws://localhost:7777", token_provider=get_token)
    await client.ready
```

### Constructor parameters

| Parameter | Type | Default | Description |
|---|---|---|---|
| `token` | `str \| None` | `None` | Static JWT. |
| `token_provider` | `Callable \| None` | `None` | Sync or async callable. Called on every connect. Supersedes `token`. |
| `reconnect` | `bool` | `True` | Auto-reconnect on drop. |
| `reconnect_delay` | `float` | `1.0` | Base delay in seconds (exponential backoff). |
| `max_reconnect_delay` | `float` | `30.0` | Maximum reconnect delay in seconds. |
| `on_connect` | `Callable \| None` | `None` | Called when connected. |
| `on_disconnect` | `Callable \| None` | `None` | Called when disconnected. |

### Subscribing

```python
from sodp.client import WatchMeta

def on_update(value, meta: WatchMeta):
    print(value, meta.version, meta.initialized)

unsub = client.watch("game.score", on_update)

# Later:
unsub()                       # remove this callback only
client.unwatch("game.score")  # cancel server subscription entirely
```

The callback may be a plain function or an `async def`. `WatchMeta` has the same fields as the TypeScript version.

### StateRef — key-scoped handle

```python
player = client.state("game.player")

unsub = player.watch(lambda v, m: print(v))
await player.set({"name": "Alice", "health": 100})
await player.patch({"health": 80})
await player.set_in("/position/x", 5)
await player.delete()
```

### Closing

```python
client.close()
```

---

## 6. State methods reference

All mutations are sent as `CALL` frames and return a `RESULT`. The TypeScript and Python convenience methods call these under the hood.

### `state.set`

Replace the entire value of a key.

```typescript
await client.set("game.score", { score: 42 });
// or:
await client.call("state.set", { state: "game.score", value: { score: 42 } });
```

### `state.patch`

Deep-merge a partial object into the existing value. Objects are merged recursively; arrays and scalars are replaced.

```typescript
// Only updates health; name, position etc. are preserved.
await client.patch("game.player", { health: 80 });

// Updates only position.x; position.y is preserved.
await client.call("state.patch", {
  state: "game.player",
  patch: { position: { x: 5 } },
});
```

### `state.set_in`

Atomically set a single nested field by [JSON Pointer](https://tools.ietf.org/html/rfc6901) path. No read-modify-write cycle needed.

```typescript
await player.setIn("/position/x", 5);
await player.setIn("/inventory/0", "new-sword");
```

```python
await player.set_in("/position/x", 5)
```

### `state.delete`

Remove a key entirely. Watchers receive a final callback with `value = null` (TS) / `None` (Python) and `meta.initialized = false`.

```typescript
await player.delete();
```

### `state.presence`

Set a nested path **and bind it to the session's lifetime**. The server automatically removes the path and broadcasts the delta when this client disconnects for any reason — preventing ghost presence entries.

```typescript
// Alice's cursor is visible to all watchers of "collab.cursors".
// When Alice's tab closes or the connection drops, her cursor disappears automatically.
await client.presence("collab.cursors", "/alice", { name: "Alice", line: 1, col: 5 });
// or via StateRef:
await cursors.presence("/alice", { name: "Alice", line: 1, col: 5 });
```

```python
await client.presence("collab.cursors", "/alice", {"name": "Alice", "line": 1})
```

---

## 7. Presence

`state.presence` is the recommended pattern for any session-scoped data: cursors, "who's online" indicators, active typing status.

**Without presence** you need a heartbeat loop + server-side TTL logic. With presence, you call it once and the server handles cleanup:

```typescript
// Once, on connect:
await client.presence("collab.cursors", `/${myUserId}`, {
  name: displayName,
  color: avatarColor,
});

// Move the cursor (just a patch — the presence binding stays):
await client.patch("collab.cursors", { [myUserId]: { line: 5, col: 12 } });
```

Multiple presence registrations on the same path are deduplicated. The most recent value wins.

---

## 8. Authentication (JWT)

When auth is enabled the server requires a JWT before it will serve any frames.

### Configuring the server

```bash
# HS256 — shared secret (development / single-host)
SODP_JWT_SECRET=my-secret sodp-server 0.0.0.0:7777

# RS256 — public key inline (recommended for production)
SODP_JWT_PUBLIC_KEY="-----BEGIN PUBLIC KEY-----\n..." sodp-server 0.0.0.0:7777

# RS256 — public key from file
SODP_JWT_PUBLIC_KEY_FILE=/etc/sodp/public.pem sodp-server 0.0.0.0:7777
```

RS256 takes priority over HS256. When none of the three variables is set, authentication is disabled and the server accepts all connections (backward compatible).

### Token requirements

- `sub` (string) — required; identifies the user; available as `session.sub` for ACL rules
- `exp` (Unix timestamp) — required; enforced with zero leeway (strictly not expired)
- Extra claims — any additional claims are forwarded to the ACL engine

### Passing tokens — TypeScript

```typescript
// Static token
const client = new SodpClient(url, { token: myJwt });

// Dynamic (e.g. refresh before expiry)
const client = new SodpClient(url, {
  tokenProvider: async () => {
    const res = await fetch("/api/refresh-token");
    return res.text();
  },
});
```

`tokenProvider` is called on every connect and reconnect, so rotating tokens are handled automatically.

### Passing tokens — Python

```python
# Static token
client = SodpClient(url, token=my_jwt)

# Dynamic
async def get_token():
    async with aiohttp.ClientSession() as s:
        resp = await s.get("/api/refresh-token")
        return await resp.text()

client = SodpClient(url, token_provider=get_token)
```

### Token expiry

The server checks the JWT `exp` claim in real time. If a session's token expires mid-connection the server sends `ERROR 401` and closes the connection. Use `tokenProvider` to obtain fresh tokens on each reconnect.

---

## 9. Access control (ACL)

### Overview

```bash
SODP_ACL_FILE=/etc/sodp/acl.json sodp-server 0.0.0.0:7777
```

When `SODP_ACL_FILE` is unset all access is allowed (backward compatible). When set, every WATCH, RESUME, and CALL frame is checked against the rules. Unauthenticated clients receive `ERROR 403` on any key that isn't open to `"*"`.

### ACL file format

```json
{
  "preset": "keycloak",
  "claim_mappings": { "tenant": "org_id" },
  "rules": [
    { "key": "public.*",       "read": "*",            "write": "*"            },
    { "key": "user.{sub}.*",   "read": "{sub}",        "write": "{sub}"        },
    { "key": "tenant.{sub}.*", "read": "tenant:{sub}", "write": "tenant:{sub}" },
    { "key": "admin.*",        "read": "role:admin",   "write": "role:admin"   }
  ]
}
```

- `preset` — optional; loads default claim-path mappings for a known IdP
- `claim_mappings` — optional; overrides or extends the preset
- `rules` — ordered list; **first matching rule wins**; keys that match no rule are denied

### Key patterns

| Segment | Meaning |
|---|---|
| `{sub}` | Matches exactly one dot-separated segment and captures its value |
| `*` | Suffix wildcard — matches **one or more** remaining segments |
| _literal_ | Must match exactly |

Examples:
- `"user.{sub}.*"` matches `user.alice.notes`, `user.bob.profile.avatar`; does **not** match `user.alice`
- `"public.*"` matches `public.board`, `public.feed.recent`; does **not** match `public`
- `"collab.doc"` matches only `collab.doc`

### Permission values

| Permission | Meaning |
|---|---|
| `"*"` | Any client (authenticated or not) |
| `"{sub}"` | `session.sub` must equal the value captured by `{sub}` in the key |
| `"role:X"` | The roles claim (via mapping) must contain the string `X` |
| `"group:X"` | The groups claim must contain `X` |
| `"perm:X"` | The permissions claim must contain `X` |
| `"tenant:{sub}"` | The tenant claim must equal the captured `{sub}` segment |
| `"KEY:VALUE"` | Any mapped claim `KEY` must contain or equal `VALUE` |
| _literal_ | `session.sub` must equal this exact string (e.g. `"admin"`) |

### Built-in IdP presets

Presets define the default mapping from short names (`role`, `group`, `tenant`, `perm`) to JWT claim paths.

| Preset | `role` path | `group` path | `tenant` path |
|---|---|---|---|
| `keycloak` | `realm_access.roles` | `groups` | `tenant_id` |
| `auth0` | `roles` | `groups` | `org_id` |
| `okta` | `groups` | `groups` | `tenant_id` |
| `cognito` | `cognito:groups` | `cognito:groups` | `custom:tenant_id` |
| `generic` | `roles` | `groups` | `tenant_id` |

All presets also map `perm` → `permissions`.

### Overriding claim paths

```json
{
  "preset": "auth0",
  "claim_mappings": {
    "role": "https://myapp.com/roles"
  },
  "rules": [
    { "key": "admin.*", "read": "role:admin", "write": "role:admin" }
  ]
}
```

Auth0 lets you namespace custom claims as URLs; the two-phase resolver handles this automatically (direct key lookup first, then dot-traversal).

### Adding a custom claim type

```json
{
  "claim_mappings": { "dept": "department" },
  "rules": [
    { "key": "hr.*", "read": "dept:HR", "write": "dept:HR" }
  ]
}
```

### Full multi-tenant Keycloak example

```json
{
  "preset": "keycloak",
  "rules": [
    { "key": "public.*",          "read": "*",            "write": "*"            },
    { "key": "user.{sub}.*",      "read": "{sub}",        "write": "{sub}"        },
    { "key": "tenant.{sub}.*",    "read": "tenant:{sub}", "write": "tenant:{sub}" },
    { "key": "admin.*",           "read": "role:admin",   "write": "role:admin"   }
  ]
}
```

JWT from Keycloak:
```json
{
  "sub": "alice",
  "realm_access": { "roles": ["user"] },
  "tenant_id": "acme"
}
```

Result:
- `public.board` — readable and writable ✅
- `user.alice.notes` — alice can read/write; bob cannot ✅
- `tenant.acme.docs` — readable/writable because `tenant_id == "acme"` ✅
- `tenant.other.docs` — denied (`tenant_id != "other"`) ✅
- `admin.config` — denied (alice lacks the `admin` role) ✅

---

## 10. Rate limiting

Limit the number of write (CALL) frames and watch (WATCH + RESUME) frames per session per second.

```bash
SODP_RATE_WRITES_PER_SEC=50  \
SODP_RATE_WATCHES_PER_SEC=20 \
sodp-server 0.0.0.0:7777
```

When a session exceeds its limit the server responds with `ERROR 429 "rate limit exceeded"` for that frame and does **not** close the connection. The client can retry after the window resets (every 1 second).

When unset, rate limiting is disabled.

---

## 11. Schema validation

The server validates every `state.set` and `state.patch` write against a SDL (Schema Definition Language) file. Invalid writes receive `ERROR 422` and are not applied.

### Schema file format

```json
{
  "game.player": {
    "type": "Object",
    "fields": {
      "name":     { "type": "String" },
      "health":   { "type": "Int" },
      "position": {
        "type": "Object",
        "fields": {
          "x": { "type": "Float" },
          "y": { "type": "Float" }
        }
      },
      "nickname": { "type": "String", "nullable": true }
    }
  },
  "game.score": "Int",
  "collab.cursors": "Any"
}
```

### Types

| Type | Description |
|---|---|
| `"String"` | JSON string |
| `"Int"` | JSON integer |
| `"Float"` | JSON number (`Int` is accepted here — widening) |
| `"Bool"` | JSON boolean |
| `"Object"` | JSON object (with optional `fields` sub-schema) |
| `"Array"` | JSON array |
| `"Any"` | Any JSON value |

### Notes

- **Permissive for undeclared keys** — keys not present in the schema file pass without validation.
- **Extra fields** — object values may contain fields not declared in the schema; they are allowed.
- **`nullable: true`** — allows `null` in addition to the declared type.
- **Float widening** — an `Int` value satisfies a `Float` field.

---

## 12. Reconnect and RESUME

The TypeScript and Python clients reconnect automatically with exponential backoff. On reconnect:

1. For keys the client has a version for, it sends `RESUME { state, since_version }`.
2. The server replays all deltas since that version from its in-memory `DeltaLog`.
3. The server finishes with a `STATE_INIT` (the "you're live" marker).
4. The client applies the replayed deltas and resumes normal operation.

If the server's delta log for a key has been truncated (log holds the last 1 000 mutations per key), the server falls back to sending a full `STATE_INIT` snapshot instead. Clients always handle this transparently — the snapshot replaces the local replica.

**No application code is needed** to handle reconnects. Subscriptions survive automatically.

### Controlling reconnect behaviour

```typescript
const client = new SodpClient(url, {
  reconnect: true,         // default
  reconnectDelay: 1000,    // ms, base for exponential backoff
  maxReconnectDelay: 30000,
  onConnect:    () => console.log("connected"),
  onDisconnect: () => console.log("disconnected"),
});
```

```python
client = SodpClient(url,
    reconnect=True,
    reconnect_delay=1.0,
    max_reconnect_delay=30.0,
    on_connect=lambda: print("connected"),
    on_disconnect=lambda: print("disconnected"),
)
```

---

## 13. Health check and metrics

### Health check

```bash
SODP_HEALTH_PORT=7778 sodp-server 0.0.0.0:7777
```

```bash
curl http://localhost:7778/
# {"status":"ok","connections":3,"version":"0.1"}
```

The health endpoint is plain HTTP (not WebSocket). Keep it internal — never expose it publicly.

Kubernetes readiness probe example:

```yaml
readinessProbe:
  httpGet:
    path: /
    port: 7778
  initialDelaySeconds: 3
  periodSeconds: 10
```

### Prometheus metrics

```bash
SODP_METRICS_PORT=9090 sodp-server 0.0.0.0:7777
```

```bash
curl http://localhost:9090/metrics
```

Key metrics:

| Metric | Description |
|---|---|
| `sodp_connections_active` | Current WebSocket connections |
| `sodp_mutations_total{method}` | Mutations by method (set / patch / set_in / delete / presence) |
| `sodp_mutation_duration_ms{method}` | Histogram of mutation latency (ms) |
| `sodp_fanout_recipients` | Histogram of subscribers per mutation |
| `sodp_rate_limited_total{type}` | Frames rejected by rate limiter (write / watch / resume) |

---

## 14. Environment variable reference

| Variable | Default | Description |
|---|---|---|
| `SODP_JWT_SECRET` | *(none)* | HS256 shared secret. Used only when neither RS256 variable is set. |
| `SODP_JWT_PUBLIC_KEY` | *(none)* | RS256 public key PEM, inline. Newlines may be escaped as `\n`. Takes priority over `SODP_JWT_SECRET`. |
| `SODP_JWT_PUBLIC_KEY_FILE` | *(none)* | Path to an RS256 public key PEM file. Used when `SODP_JWT_PUBLIC_KEY` is absent. |
| `SODP_ACL_FILE` | *(none)* | Path to a JSON ACL file. When unset all access is allowed. |
| `SODP_RATE_WRITES_PER_SEC` | *(unlimited)* | Maximum CALL frames per session per second. |
| `SODP_RATE_WATCHES_PER_SEC` | *(unlimited)* | Maximum WATCH + RESUME frames per session per second. |
| `SODP_HEALTH_PORT` | *(none)* | Port for the plain HTTP health endpoint. |
| `SODP_METRICS_PORT` | *(none)* | Port for the Prometheus `/metrics` endpoint. |
| `SODP_MAX_CONNECTIONS` | *(unlimited)* | Maximum concurrent WebSocket connections. New connections receive a 503 when the limit is reached. |
| `SODP_MAX_FRAME_BYTES` | `1048576` | Maximum inbound frame size (bytes). Frames exceeding this receive `ERROR 413` and the connection is closed. |
| `SODP_WS_PING_INTERVAL` | `25` | WebSocket protocol-level ping interval (seconds). Sessions that do not respond with a pong before the next tick are closed. |
| `RUST_LOG` | `info` | Log verbosity (`error`, `warn`, `info`, `debug`, `trace`). |
