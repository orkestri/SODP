# State-Oriented Data Protocol (SODP)

## Protocol Specification — Version 1.0

**Status:** Stable
**Supersedes:** docs/ref_v1.md through docs/ref_v6.md, docs/protocol.md
**Date:** 2026-04-18
**License:** MIT

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Terminology](#2-terminology)
3. [Transport](#3-transport)
4. [Frame Format](#4-frame-format)
5. [Frame Types](#5-frame-types)
6. [State Synchronization](#6-state-synchronization)
7. [Delta Operations](#7-delta-operations)
8. [RPC Layer](#8-rpc-layer)
9. [Subscription Lifecycle](#9-subscription-lifecycle)
10. [Authentication and Authorization](#10-authentication-and-authorization)
11. [Error Handling](#11-error-handling)
12. [Operational Features](#12-operational-features)
13. [Persistence](#13-persistence)
14. [Horizontal Scaling](#14-horizontal-scaling)
15. [Schema Validation](#15-schema-validation)
16. [Security Considerations](#16-security-considerations)
17. [IANA Considerations](#17-iana-considerations)
18. [Compliance Checklist](#18-compliance-checklist)

---

## 1. Introduction

### 1.1 Purpose

The State-Oriented Data Protocol (SODP) defines a mechanism for continuous
state synchronization between a server and one or more clients over a
persistent bidirectional connection.

Instead of the traditional request-response cycle, SODP operates on a
subscribe-and-stream model:

1. A client subscribes to a named state key.
2. The server delivers the current value as a full snapshot.
3. Subsequent mutations are delivered as incremental deltas containing only
   the changed fields.
4. All connected subscribers receive every mutation in real time.

SODP integrates a lightweight RPC layer for mutations, so reads and writes
flow over a single connection with a unified frame format.

### 1.2 Design Goals

- **Minimal wire overhead.** Binary encoding (MessagePack) and structural
  deltas reduce bandwidth by an order of magnitude compared to
  full-document retransmission.
- **Transparent reconnection.** Clients track the last-seen version per key
  and request replay of missed deltas on reconnect.
- **Transport independence.** The frame format is defined independently of
  the underlying transport. This specification binds to WebSocket; future
  versions MAY define bindings for raw TCP, HTTP/2 streams, WebTransport,
  or a dedicated `sodp://` URI scheme.
- **Correctness by default.** The server is the single source of truth.
  Clients maintain partial replicas that converge deterministically.

### 1.3 Scope

This specification covers:

- Wire format and frame types
- State synchronization semantics (WATCH, DELTA, RESUME)
- RPC semantics (CALL, RESULT)
- Authentication and per-key authorization
- Error handling and recovery
- Operational features (heartbeat, rate limiting, backpressure)
- Optional persistence and horizontal scaling
- Optional schema validation

This specification does NOT cover:

- Client SDK API design (implementation-specific)
- Server internals beyond observable behavior
- Deployment topology
- Planned features not yet implemented (CRDTs, edge federation, views)

---

## 2. Terminology

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in [RFC 2119][rfc2119].

| Term | Definition |
|---|---|
| **State key** | A dot-separated string (e.g., `game.score`, `user.alice.profile`) that identifies a single piece of synchronized state. |
| **State value** | The current value associated with a state key. MAY be any JSON-compatible type: object, array, string, number, boolean, or null. |
| **Version** | A monotonically increasing 64-bit unsigned integer assigned to each mutation. Versions are global (shared across all keys on a single node) and strictly increasing. |
| **Delta** | An ordered list of structural operations (ADD, UPDATE, REMOVE) that transform a state value from one version to the next. |
| **Stream** | A logical channel within a connection. Stream 0 is the control stream; streams >= 10 are subscription streams allocated by the client. |
| **Session** | The server-side representation of a single client connection, including its authentication state, subscriptions, and rate-limiting counters. |
| **Frame** | The atomic unit of communication: a 4-element MessagePack array. |
| **Subscriber** | A session that has an active WATCH on a given state key. |
| **Partial State Replica (PSR)** | The client-side cache of watched state values. Eventually consistent with the server. |

[rfc2119]: https://www.rfc-editor.org/rfc/rfc2119

---

## 3. Transport

### 3.1 Transport Requirements

SODP requires a transport that provides:

1. **Bidirectional communication** — both client and server send frames.
2. **Binary payload support** — frames are MessagePack-encoded byte sequences.
3. **Ordered delivery** — frames MUST arrive in the order they were sent.
4. **Persistent connection** — the connection remains open for the duration of the session.

### 3.2 WebSocket Binding

The reference transport binding is [WebSocket (RFC 6455)][rfc6455].

- All SODP frames MUST be sent as WebSocket binary messages (opcode `0x02`).
- Text frames MUST NOT be used for SODP communication.
- Fragmented WebSocket messages MAY be used by the transport layer; the SODP
  frame boundary is defined by the WebSocket message boundary, not by
  MessagePack framing.
- Implementations SHOULD set `TCP_NODELAY` on the underlying socket to
  minimize latency.

[rfc6455]: https://www.rfc-editor.org/rfc/rfc6455

### 3.3 TLS

Production deployments SHOULD use WebSocket over TLS (`wss://`).
Implementations SHOULD support TLS 1.2 or later. Certificate validation
MUST follow standard HTTPS rules ([RFC 6125][rfc6125]).

[rfc6125]: https://www.rfc-editor.org/rfc/rfc6125

### 3.4 Future Transport Bindings

This specification anticipates future bindings for:

- Raw TCP with length-prefixed MessagePack frames
- HTTP/2 bidirectional streaming
- WebTransport (QUIC-based)
- A dedicated `sodp://` / `sodps://` URI scheme (see [Section 17](#17-iana-considerations))

The frame format (Section 4) is transport-independent. A future transport
binding specification MUST define how SODP frames are framed, how
connections are established, and how TLS is negotiated.

---

## 4. Frame Format

### 4.1 Wire Encoding

Every SODP message is a **4-element MessagePack array**:

```
[ frame_type, stream_id, seq, body ]
```

MessagePack encoding MUST follow the [MessagePack specification][msgpack]
(2013 revision, commonly called "MessagePack 2.0"). Integers MUST use the
most compact encoding available.

[msgpack]: https://github.com/msgpack/msgpack/blob/master/spec.md

### 4.2 Frame Fields

| Field | Type | Description |
|---|---|---|
| `frame_type` | uint 8 | Frame type identifier (Section 5). |
| `stream_id` | uint 32 | Logical stream. `0` = control stream; `>= 10` = subscription stream. |
| `seq` | uint 64 | Monotonically increasing sequence number, scoped per connection per direction. MAY be `0` for server frames that do not require acknowledgment. |
| `body` | map or null | Frame-type-specific payload. MUST be a MessagePack map or nil. |

### 4.3 Stream Allocation

| Range | Owner | Purpose |
|---|---|---|
| `0` | Shared | Control stream: HELLO, AUTH, AUTH_OK, CALL, RESULT, ERROR, HEARTBEAT. |
| `1` – `9` | Reserved | Reserved for future use. Implementations MUST NOT use these stream IDs. |
| `>= 10` | Client | Subscription streams. The client chooses a stream ID when sending WATCH or RESUME. The server echoes the actual stream ID in STATE_INIT. |

The server MAY reassign the client-suggested stream ID. Clients MUST
route incoming STATE_INIT and DELTA frames by the `state` field in the
body (the key name), NOT by stream ID alone. The client SHOULD maintain
a `stream_id -> key` mapping populated from STATE_INIT bodies.

### 4.4 Sequence Numbers

Each direction (client-to-server, server-to-client) maintains an
independent monotonically increasing sequence counter. Implementations
SHOULD increment `seq` by 1 for each frame sent. Receivers MAY use `seq`
to detect dropped frames but MUST NOT rely on contiguous values.

---

## 5. Frame Types

### 5.1 Frame Type Registry

| ID | Name | Direction | Description |
|---|---|---|---|
| `0x01` | HELLO | S -> C | Server greeting; announces protocol version and capabilities. |
| `0x02` | WATCH | C -> S | Subscribe to one or more state keys. |
| `0x03` | STATE_INIT | S -> C | Full snapshot of a state key's current value. |
| `0x04` | DELTA | S -> C | Incremental change to a watched state key. |
| `0x05` | CALL | C -> S | Invoke a server-side method. |
| `0x06` | RESULT | S -> C | Response to a CALL. |
| `0x07` | ERROR | S -> C | Error notification. |
| `0x08` | *(reserved)* | — | Reserved for future use. |
| `0x09` | HEARTBEAT | Both | Keep-alive ping/pong. |
| `0x0A` | RESUME | C -> S | Reconnect and request replay of missed deltas. |
| `0x0B` | AUTH | C -> S | Send a JWT for authentication. |
| `0x0C` | AUTH_OK | S -> C | Authentication accepted. |
| `0x0D` | UNWATCH | C -> S | Cancel a subscription. |

Frame type IDs `0x0E` through `0xFF` are reserved for future use.

### 5.2 HELLO (0x01) — Server -> Client

Sent immediately after the WebSocket connection is established. The client
MUST NOT send any frames (except HEARTBEAT) before receiving HELLO.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `protocol` | string | REQUIRED | Protocol identifier. MUST be `"sodp"`. |
| `version` | string | REQUIRED | Protocol version. This specification defines `"1.0"`. |
| `server` | string | RECOMMENDED | Server implementation name (e.g., `"SODP-RS"`). |
| `auth` | boolean | REQUIRED | `true` if the server requires authentication before the session becomes live. `false` if the session is immediately live. |
| `capabilities` | map | RECOMMENDED | Server-advertised configuration (see below). |

**Capabilities object:**

| Field | Type | Description |
|---|---|---|
| `rate_limit_writes` | uint | Maximum write operations per second per session. Absent if no limit. |
| `rate_limit_watches` | uint | Maximum WATCH/RESUME operations per second per session. Absent if no limit. |
| `backpressure_limit` | uint | Bounded channel capacity per session (number of frames). |
| `multi_watch` | boolean | `true` if the server supports multi-key WATCH (`states` field). |
| `params` | boolean | `true` if the server supports the `params` field on WATCH/RESUME. |
| `resume` | boolean | `true` if the server supports the RESUME frame. |

Servers MAY include additional implementation-specific fields in
`capabilities`. Clients MUST ignore unknown fields.

**Example:**

```
[0x01, 0, 0, {
  "protocol": "sodp",
  "version": "1.0",
  "server": "SODP-RS",
  "auth": true,
  "capabilities": {
    "rate_limit_writes": 50,
    "rate_limit_watches": 10,
    "backpressure_limit": 1024,
    "multi_watch": true,
    "params": true,
    "resume": true
  }
}]
```

### 5.3 AUTH (0x0B) — Client -> Server

Sent after receiving HELLO with `auth: true`.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `token` | string | REQUIRED | A signed JWT string. |

The server MUST validate the token before the session becomes live
(see [Section 10](#10-authentication-and-authorization)).

### 5.4 AUTH_OK (0x0C) — Server -> Client

Sent after successful JWT validation.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `sub` | string | REQUIRED | The authenticated subject (from the JWT `sub` claim). |

After AUTH_OK, the session is live. The client MAY send WATCH, RESUME,
CALL, UNWATCH, and HEARTBEAT frames.

### 5.5 WATCH (0x02) — Client -> Server

Subscribe to one or more state keys.

**Body fields (single-key):**

| Field | Type | Required | Description |
|---|---|---|---|
| `state` | string | REQUIRED | The state key to watch. |
| `params` | map | OPTIONAL | Opaque metadata echoed back in STATE_INIT. |

**Body fields (multi-key):**

| Field | Type | Required | Description |
|---|---|---|---|
| `states` | array of string | REQUIRED | List of state keys to watch. |
| `params` | map | OPTIONAL | Shared metadata echoed in each STATE_INIT. |

The `state` and `states` fields are mutually exclusive. A WATCH frame
MUST contain exactly one of them.

For a multi-key WATCH, the server decomposes the request into N individual
subscriptions. Each key receives its own `stream_id` and its own
STATE_INIT response. If any key is denied by ACL, the server MUST send
ERROR 403 for that key; remaining keys MUST proceed normally.

The `stream_id` in the client's WATCH frame is a suggestion. The server
MAY use it or assign a different one; the authoritative mapping is
established by the `state` field in the STATE_INIT response.

### 5.6 STATE_INIT (0x03) — Server -> Client

Delivers the full current value of a state key. Sent in response to
WATCH, and as the final frame of a RESUME replay sequence.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `state` | string | REQUIRED | The state key this snapshot represents. |
| `version` | uint 64 | REQUIRED | The version of this key at the time the snapshot was taken. |
| `value` | any | REQUIRED | The current value. MAY be null if the key has never been written. |
| `initialized` | boolean | REQUIRED | `false` when the key has never been written to the server (value is null). `true` otherwise. |
| `params` | map | OPTIONAL | Echoed from the original WATCH/RESUME frame, if present. |

The `stream_id` in the frame header is the server-assigned subscription
stream for this key. All subsequent DELTA frames for this key on this
session use the same `stream_id`.

STATE_INIT is also used as the "you are now live" marker after a RESUME
replay. See [Section 9.4](#94-resume).

### 5.7 DELTA (0x04) — Server -> Client

Delivers an incremental change to a watched state key.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `version` | uint 64 | REQUIRED | The new version of this key after the mutation. |
| `ops` | array of DeltaOp | REQUIRED | Ordered list of structural operations. See [Section 7](#7-delta-operations). |

The `stream_id` in the frame header identifies which subscription
(and therefore which key) this DELTA applies to.

A DELTA frame MAY also include a `state` field containing the key name.
This is OPTIONAL for single-key subscriptions (the client can route by
`stream_id`), but RECOMMENDED for implementations to include for
debugging and for future prefix-watch support.

### 5.8 CALL (0x05) — Client -> Server

Invoke a server-side method. Always sent on stream 0 (control stream).

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `call_id` | string | REQUIRED | Client-generated UUID for correlating the RESULT response. |
| `method` | string | REQUIRED | Method name (see [Section 8](#8-rpc-layer)). |
| `args` | map | REQUIRED | Method-specific arguments. |

### 5.9 RESULT (0x06) — Server -> Client

Response to a CALL. Always sent on stream 0.

**Body fields (success):**

| Field | Type | Required | Description |
|---|---|---|---|
| `call_id` | string | REQUIRED | Echoed from the CALL. |
| `success` | boolean | REQUIRED | `true`. |
| `data` | any | REQUIRED | Method-specific result data. For state mutations, this is `{ "version": <uint64> }`. |

**Body fields (failure):**

| Field | Type | Required | Description |
|---|---|---|---|
| `call_id` | string | REQUIRED | Echoed from the CALL. |
| `success` | boolean | REQUIRED | `false`. |
| `data` | string | REQUIRED | Human-readable error description. |

### 5.10 ERROR (0x07) — Server -> Client

Error notification not tied to a specific CALL. Used for ACL denials
on WATCH, rate limiting, invalid frames, and protocol violations.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `code` | uint 32 | REQUIRED | Error code (see [Section 11](#11-error-handling)). |
| `message` | string | REQUIRED | Human-readable error description. |

The `stream_id` in the frame header SHOULD be the stream that caused the
error, or `0` for session-level errors.

### 5.11 HEARTBEAT (0x09) — Bidirectional

Keep-alive frame. Body MUST be null.

```
[0x09, 0, 0, null]
```

The server MUST send a HEARTBEAT at a regular interval (RECOMMENDED:
every 30 seconds). The client MUST respond to each server HEARTBEAT
with a HEARTBEAT of its own. The server SHOULD close the connection if
no HEARTBEAT response is received within the next heartbeat interval.

The server MUST also respond to client-initiated HEARTBEAT frames with
a HEARTBEAT.

HEARTBEAT frames are permitted during the authentication handshake
(between HELLO and AUTH_OK).

In addition to application-level heartbeats, WebSocket implementations
SHOULD send transport-level Ping frames on the same schedule.

### 5.12 RESUME (0x0A) — Client -> Server

Request replay of missed deltas after a reconnect.

**Body fields:**

| Field | Type | Required | Description |
|---|---|---|---|
| `state` | string | REQUIRED | The state key to resume. |
| `since_version` | uint 64 | REQUIRED | The last version the client received for this key. |
| `params` | map | OPTIONAL | Echoed in the final STATE_INIT, same as WATCH. |

See [Section 9.4](#94-resume) for server behavior.

### 5.13 UNWATCH (0x0D) — Client -> Server

Cancel a subscription. Always sent on stream 0.

**Body fields (single-key):**

| Field | Type | Required | Description |
|---|---|---|---|
| `state` | string | REQUIRED | The state key to unwatch. |

**Body fields (multi-key):**

| Field | Type | Required | Description |
|---|---|---|---|
| `states` | array of string | REQUIRED | List of state keys to unwatch. |

After UNWATCH, the server MUST stop sending DELTA and STATE_INIT frames
for the specified key(s) to this session.

---

## 6. State Synchronization

### 6.1 State Model

Each state key is an independent, mutable value identified by a
dot-separated string. Key names MUST match the pattern:

```
[a-zA-Z0-9_]+(\.[a-zA-Z0-9_]+)*
```

Examples: `game.score`, `user.alice.profile`, `runs.abc123.nodes.n1`.

### 6.2 Values

A state value MAY be any JSON-compatible type: object, array, string,
number, boolean, or null. The server stores values internally as
MessagePack `Value` types.

A key that has never been written has the value `null` and
`initialized = false`.

### 6.3 Versioning

Every state key has an associated version number. The version is drawn
from a monotonically increasing global counter shared across all keys
on a single server node.

Properties:

- Versions are strictly increasing per key (no duplicates).
- Versions are NOT necessarily contiguous per key (gaps appear when other
  keys are mutated between two writes to this key).
- Version `0` indicates the key has never been written.
- The global counter uses 64-bit unsigned integers. Overflow is not a
  practical concern.

### 6.4 Consistency Model

**Single-node:** All mutations are totally ordered by the global version
counter. Subscribers receive DELTA frames in version order. This provides
linearizable reads for any single key.

**Multi-node (with Redis):** Each node maintains an independent version
counter. Cross-node synchronization uses last-write-wins semantics based
on version comparison. The consistency model is **eventual**: two nodes
MAY temporarily hold different values for the same key, but will converge
once the Redis sync completes. See [Section 14](#14-horizontal-scaling).

### 6.5 Concurrent Writes

When two clients write to the same key concurrently, the server applies
mutations in the order they arrive. The second write sees the result of
the first write as its "old" value for delta computation. Both writes
produce DELTA frames delivered to all subscribers in order. There is no
conflict detection or merge — last write wins.

---

## 7. Delta Operations

### 7.1 DeltaOp Format

A DeltaOp is a MessagePack map with the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `op` | string | REQUIRED | One of `"ADD"`, `"UPDATE"`, or `"REMOVE"`. Case-sensitive. |
| `path` | string | REQUIRED | JSON Pointer ([RFC 6901][rfc6901]) identifying the target location. |
| `value` | any | Conditional | The new value. REQUIRED for ADD and UPDATE. MUST NOT be present for REMOVE. |

Implementations receiving an unknown `op` value MUST reject the delta
(throw an error, not silently ignore). Silent ignoring of unknown ops
causes the client's cached state to diverge from the server.

[rfc6901]: https://www.rfc-editor.org/rfc/rfc6901

### 7.2 Op Types

| Op | Meaning | When Used |
|---|---|---|
| `ADD` | A new field or element that did not exist in the previous value. | Field added to an object; element appended to an array; root value created for the first time. |
| `UPDATE` | An existing field or element whose value changed. | Field value replaced in an object; element replaced in an array; root value replaced entirely. |
| `REMOVE` | A field, element, or root value that was deleted. | Field removed from an object; element removed from an array; key deleted entirely. |

### 7.3 Path Format

Paths follow [JSON Pointer (RFC 6901)][rfc6901]:

- `"/"` — the root value itself. An ADD or UPDATE at root replaces the
  entire state value. A REMOVE at root deletes the key.
- `"/field"` — a top-level field named `field`.
- `"/a/b/c"` — the field `c` inside object `b` inside object `a`.

### 7.4 Array Semantics

The following array-specific path semantics MUST be supported:

**Append (`/-`):** The special token `-` at the end of a path references
the element past the last index of an array, per RFC 6901 Section 4.
An ADD or UPDATE with path `/-` (or `/parent/-` for nested arrays)
appends the value to the end of the array. A REMOVE with path `/-`
removes the last element.

```json
// applyOps([1, 2, 3], [{"op": "ADD", "path": "/-", "value": 4}])
// Result: [1, 2, 3, 4]
```

**Numeric index (`/N`):** A purely numeric path segment on an array
references the element at that index (0-based). ADD and UPDATE replace
the element at that index. REMOVE splices the element out (shifts
subsequent elements down).

```json
// applyOps([1, 2, 3], [{"op": "REMOVE", "path": "/1"}])
// Result: [1, 3]
```

**Null/missing state materialization:** When the current state is null,
undefined, or not a container (e.g., the key has never been written),
and the path targets an array-like location (the next path segment is
`-` or purely numeric), implementations MUST initialize the root as an
empty array, not an empty object. For object-like paths, the root MUST
be initialized as an empty object.

```json
// applyOps(null, [{"op": "ADD", "path": "/-", "value": "x"}])
// Result: ["x"]  (NOT: {"-": "x"})

// applyOps(null, [{"op": "ADD", "path": "/items/-", "value": "x"}])
// Result: {"items": ["x"]}
```

The same materialization rule applies to intermediate nodes when
navigating nested paths: if a path segment requires descending into a
node that is null or missing, the implementation MUST create an array
if the *next* segment is `-` or numeric, otherwise an object.

### 7.5 Structural Diff Algorithm

The server computes deltas by comparing old and new values:

1. Fields present in the new value but absent in the old -> `ADD`.
2. Fields present in both with different values -> recurse into objects;
   emit `UPDATE` for scalars and arrays.
3. Fields present in the old value but absent in the new -> `REMOVE`.

Complexity is `O(changed_fields)`, not `O(total_fields)`.

Arrays are compared atomically in the reference implementation: if the
array value changed, the entire array is emitted as a single UPDATE op.
Implementations MAY emit fine-grained per-element ops (using `/-` and
numeric indices), but MUST NOT assume that all servers do so.

### 7.6 Ordering

Ops within a single DELTA frame MUST be applied in order. The resulting
state after applying all ops MUST match the server's state at the
advertised version.

### 7.7 Immutability

Delta application MUST NOT mutate the input state. Implementations MUST
clone or copy-on-write before applying ops, so that callers holding
references to the previous value see no changes.

---

## 8. RPC Layer

### 8.1 Invocation

All RPC invocations use CALL frames on stream 0. The `call_id` is a
client-generated UUID (RFC 4122) used to correlate the RESULT response.
The server MUST respond with exactly one RESULT per CALL.

Multiple CALLs MAY be in flight concurrently. The server MAY process
them in any order but MUST respond in order of completion.

### 8.2 Built-in Methods

| Method | Required Args | Effect |
|---|---|---|
| `state.set` | `state`: string, `value`: any | Replace the full value of the state key. |
| `state.patch` | `state`: string, `patch`: map | Shallow-merge `patch` into the existing value. Objects are merged at the top level; arrays and scalars are replaced. |
| `state.set_in` | `state`: string, `path`: string, `value`: any | Set a single nested field identified by a JSON Pointer path. Creates intermediate objects as needed. |
| `state.delete` | `state`: string | Remove the state key entirely. Subscribers receive a DELTA with `[{"op": "REMOVE", "path": "/"}]`. |
| `state.presence` | `state`: string, `path`: string, `value`: any | Set a nested path and bind it to the session's lifetime. When the session ends (disconnect, timeout, or server shutdown), the server MUST automatically REMOVE the path and broadcast the resulting DELTA to all subscribers. |

### 8.3 Mutation Pipeline

Each state mutation follows this pipeline:

```
1. Rate limiter check         -> ERROR 429 if exceeded
2. Authentication check       -> ERROR 401 if unauthenticated
3. ACL authorization check    -> ERROR 403 if denied
4. Schema validation          -> ERROR 422 if invalid
5. State mutation (atomic)
6. Delta computation
7. Persistence (if enabled)
8. Fanout broadcast to subscribers
9. RESULT to the calling session
10. Cross-node sync (if Redis enabled)
```

Steps 5 through 7 are atomic per mutation: if any step fails, the
mutation is rejected and no state change occurs.

### 8.4 Custom Methods

Implementations MAY define custom methods beyond the built-in set.
A CALL with an unknown method name MUST receive ERROR 404.

---

## 9. Subscription Lifecycle

### 9.1 WATCH -> STATE_INIT Flow

1. Client sends WATCH with a state key.
2. Server registers the subscription and assigns a stream ID.
3. Server sends STATE_INIT on the assigned stream with the current value.
4. Server begins sending DELTA frames on the same stream for all
   subsequent mutations to that key.

### 9.2 Multi-key WATCH

A WATCH frame with a `states` array subscribes to multiple keys in a
single frame. The server MUST decompose this into N independent
subscriptions, each with its own stream ID and STATE_INIT response.

If one key is denied by ACL, the server MUST send ERROR 403 for that
key only; the remaining keys MUST proceed.

### 9.3 Shared Subscriptions

Multiple WATCH calls for the same key on the same session SHOULD share
a single server-side subscription. The server sends one copy of each
DELTA; the client routes it to all registered callbacks.

### 9.4 RESUME

On reconnect, a client that previously watched a key and knows its last
version SHOULD send RESUME instead of WATCH.

**Server behavior:**

1. Look up all stored deltas for the key with version > `since_version`.
2. If deltas are available, replay them as individual DELTA frames in
   version order.
3. After the replay (or if no deltas are available because they were
   evicted), send STATE_INIT with the current value as the "live" marker.

The STATE_INIT at the end of a RESUME sequence indicates that the client
is fully caught up and will now receive live DELTA frames. The client
MUST NOT assume that receiving STATE_INIT means deltas were missed; it
simply means the replay phase is complete.

**Delta log capacity:** The server maintains a per-key delta log capped
at 1,000 entries by default. If the client's `since_version` references
a version older than the oldest entry in the log, the server MUST skip
the replay and send STATE_INIT directly. Clients MUST handle this
transparently — it is semantically equivalent to a fresh WATCH.

### 9.5 UNWATCH

Cancels a subscription. After UNWATCH, the server MUST stop sending
DELTA and STATE_INIT frames for the specified key to this session and
MUST remove the subscriber from the fanout registry.

Multi-key UNWATCH (with a `states` array) cancels multiple subscriptions
in a single frame.

### 9.6 Reconnection Strategy

Clients SHOULD implement automatic reconnection with exponential backoff:

1. Base delay: 1 second.
2. Multiplier: 2x per attempt.
3. Maximum delay: 30 seconds.
4. Jitter: add a random component (RECOMMENDED: 0-500ms) to avoid
   thundering herd on server restart.

On reconnect:

- Keys with a known version (version > 0) -> send RESUME.
- Keys with no known version -> send WATCH.
- All pending CALLs SHOULD be rejected with a connection-lost error.

---

## 10. Authentication and Authorization

### 10.1 Authentication

When the server's HELLO frame has `auth: true`, the client MUST send an
AUTH frame before the session becomes live. The server MUST NOT process
WATCH, RESUME, CALL, or UNWATCH frames until authentication succeeds.
HEARTBEAT frames are permitted during the handshake.

**JWT validation:**

- The server MUST support at least one of HS256 or RS256.
- The JWT MUST contain a `sub` (subject) claim and an `exp`
  (expiration) claim.
- The server MUST reject tokens with an `exp` in the past.
- On successful validation, the server sends AUTH_OK and extracts the
  JWT claims for use in ACL evaluation.
- On failure, the server sends ERROR with code 401 and closes the
  WebSocket connection.

**Live expiry enforcement:**

The server MUST check token expiry continuously. When the JWT's `exp`
timestamp is reached during an active session, the server MUST send
ERROR 401 and close the connection. There is no grace period.

Implementations SHOULD check expiry at least once per heartbeat interval.

### 10.2 Authorization (ACL)

Per-key access control is defined in a JSON rules file. When no ACL file
is configured, all access is allowed (backward compatible).

**Rule format:**

```json
{
  "rules": [
    {
      "pattern": "<key-pattern>",
      "read": "<permission>",
      "write": "<permission>"
    }
  ],
  "preset": "<idp-name>",
  "claim_mappings": { ... }
}
```

Rules are evaluated in order; the first matching rule wins.

### 10.3 Key Pattern Grammar

Key patterns use dot-separated segments with two special forms:

```
pattern     = segment ("." segment)*
segment     = literal | capture | wildcard
literal     = 1*(ALPHA / DIGIT / "_")
capture     = "{sub}"
wildcard    = "*"
```

- **Literal** segments match exactly.
- **`{sub}`** matches any single segment and requires the segment value
  to equal the session's JWT `sub` claim.
- **`*`** matches one or more remaining segments (suffix wildcard only;
  MUST be the last segment in the pattern).

Examples:

| Pattern | Matches | Does Not Match |
|---|---|---|
| `game.score` | `game.score` | `game.scores`, `game` |
| `user.{sub}` | `user.alice` (if sub=alice) | `user.bob` (if sub=alice), `user.alice.profile` |
| `game.*` | `game.score`, `game.chat`, `game.a.b.c` | `game`, `games.x` |

### 10.4 Permission Syntax

| Permission | Meaning |
|---|---|
| `"*"` | Allow any authenticated session. |
| `"{sub}"` | Allow only the session whose JWT `sub` matches. |
| `"role:X"` | Require role `X` in the session's claims. |
| `"group:X"` | Require group `X` in the session's claims. |
| `"perm:X"` | Require permission `X` in the session's claims. |
| `"tenant:{sub}"` | Require the session's `sub` to match a tenant isolation field. |
| `"KEY:VALUE"` | Match an arbitrary claim `KEY` with value `VALUE`. |

### 10.5 Claim Resolution

Claims are resolved in two phases:

1. **Direct key lookup:** Look up the claim key directly in the JWT's
   claims map (handles keys with special characters like `cognito:groups`
   or URL-prefixed namespaces).
2. **Dot-traversal:** If direct lookup fails, split the claim path on `.`
   and traverse nested objects (handles keys like `realm_access.roles`).

### 10.6 Identity Provider Presets

Built-in presets configure claim paths for common providers:

| Preset | Roles Path | Groups Path |
|---|---|---|
| `keycloak` | `realm_access.roles` | `groups` |
| `auth0` | `permissions` | `org_id` |
| `okta` | `groups` | `groups` |
| `cognito` | `cognito:groups` | `cognito:groups` |
| `generic` | `roles` | `groups` |

Custom `claim_mappings` in the ACL file override preset-defined paths.

---

## 11. Error Handling

### 11.1 Error Code Registry

| Code | Name | Description | Connection |
|---|---|---|---|
| `400` | Bad Request | Malformed frame, missing required field, invalid body. | Stays open |
| `401` | Unauthorized | Invalid, expired, or missing JWT. | Closed |
| `403` | Forbidden | ACL denies access to the requested key. | Stays open |
| `404` | Not Found | Unknown method name in CALL. | Stays open |
| `422` | Unprocessable | Schema validation failed on a mutation. | Stays open |
| `429` | Rate Limited | Per-session rate limit exceeded. | Stays open |
| `500` | Internal Error | Unexpected server error. | Implementation-specific |

### 11.2 Connection-Closing Errors

Errors with code `401` MUST be followed by a WebSocket close.
Implementations SHOULD NOT automatically reconnect after a 401 (the
token is invalid, not the connection).

All other error codes leave the connection alive. The client MAY retry
the failed operation.

### 11.3 Error Correlation

ERROR frames do not currently include a `call_id` field. For errors
caused by CALL operations, the server sends a RESULT with
`success: false` instead. ERROR frames are used for subscription-level
and session-level errors (ACL denial on WATCH, rate limiting, etc.).

### 11.4 Client Error Handling

Clients SHOULD surface errors programmatically (e.g., via an `onError`
callback or event). Clients MUST NOT silently discard ERROR frames.
Transport-level WebSocket errors (connection refused, TLS failure) and
protocol-level ERROR frames SHOULD be delivered through the same error
channel for consistent handling.

---

## 12. Operational Features

### 12.1 Heartbeat

The server MUST send HEARTBEAT frames at a configurable interval
(RECOMMENDED default: 30 seconds). The client MUST echo each
HEARTBEAT back. The server SHOULD close the connection if no response
is received within the next heartbeat interval.

### 12.2 Rate Limiting

Servers MAY enforce per-session rate limits:

- **Write rate limit:** Maximum CALL operations per second. Exceeding
  this limit MUST result in ERROR 429.
- **Watch rate limit:** Maximum WATCH and RESUME operations per second.
  Exceeding this limit MUST result in ERROR 429.

Rate limits use a fixed 1-second window. The current limits are
advertised in HELLO `capabilities`.

### 12.3 Backpressure

Each subscriber has a bounded outbound channel (default capacity: 1,024
frames, configurable). When a slow client's channel is full:

1. The server MUST evict the subscriber immediately.
2. The server MUST close the WebSocket connection.
3. The client's auto-reconnect triggers RESUME, recovering missed state.

This design prevents a single slow client from causing unbounded memory
growth on the server. The channel capacity is advertised in HELLO
`capabilities.backpressure_limit`.

### 12.4 Health Endpoint

Servers SHOULD expose an HTTP health endpoint on a configurable port:

```
GET /health

200 OK
Content-Type: application/json

{ "status": "ok", "connections": 42, "version": "1.0" }
```

### 12.5 Metrics

Servers SHOULD expose Prometheus-format metrics on a configurable port.
Metric names and labels are implementation-specific.

### 12.6 Graceful Shutdown

On receiving SIGTERM (or equivalent), the server SHOULD:

1. Stop accepting new connections.
2. Notify all active sessions (implementation-specific).
3. Flush any pending persistence writes.
4. Close all WebSocket connections.

---

## 13. Persistence

Persistence is OPTIONAL. Servers MAY operate in ephemeral mode (all state
lost on restart) or persistent mode.

### 13.1 Segmented Log

The reference persistence model is a segmented append-only log on disk:

```
log_dir/
  seg_0000000000.log
  seg_0000000001.log
  ...
```

Each log entry is a MessagePack-encoded record:

```
{ "version": <uint64>, "key": "<string>", "ops": [<DeltaOp>, ...], "value": <any> }
```

### 13.2 Recovery

On startup, the server MUST:

1. Read all segments in filename order.
2. Sort entries by version (safety net for partial writes from crashes).
3. Rebuild the in-memory state store (last write per key wins).
4. Rebuild per-key delta logs for RESUME support.

### 13.3 Compaction

When the number of segments exceeds a threshold (RECOMMENDED: 3), the
server SHOULD compact by:

1. Snapshotting all live keys (one entry per key, `ops = []`).
2. Writing the snapshot as a new segment.
3. Removing old segments.

Compaction preserves all live state but discards the delta history.
After compaction, RESUME requests referencing pre-compaction versions
fall back to STATE_INIT (Section 9.4).

---

## 14. Horizontal Scaling

Horizontal scaling is OPTIONAL. When not configured, the server operates
in single-node mode.

### 14.1 Redis-Based Scaling

The reference horizontal scaling model uses Redis for cross-node state
sharing and delta fanout.

**State storage:**

```
HSET sodp:state {key} rmp_serde((version, value))
```

All nodes share state via a Redis hash. Each mutation is synced
asynchronously (fire-and-forget) to avoid adding latency to the hot path.

**Cross-node fanout:**

```
PUBLISH sodp:delta:{key} rmp_serde((node_id, body_mp))
```

Each node subscribes to `sodp:delta:*` via a dedicated connection. On
receiving a message, the node delivers the delta to local subscribers,
skipping messages from its own `node_id`.

**Startup sync:**

On startup, each node loads all state from Redis via `HGETALL sodp:state`.
When the Redis version for a key is strictly higher than the local
version, the Redis value wins.

### 14.2 Consistency

Multi-node SODP provides **eventual consistency**:

- Writes are applied locally first, then synced to Redis asynchronously.
- Two nodes writing the same key concurrently MAY temporarily hold
  different values. Convergence occurs when both nodes process the
  other's Redis PUBLISH.
- The higher version always wins in conflict resolution.

### 14.3 Cross-Node RESUME

RESUME replays from the local node's delta log only. If the client
reconnects to a different node, the delta log may not contain the
expected entries. In this case, the server falls back to STATE_INIT.
Clients MUST handle this transparently.

### 14.4 Failure Modes

| Failure | Behavior |
|---|---|
| Redis unavailable | Nodes continue operating independently. Cross-node fanout paused. State divergence until Redis recovers. |
| Node failure | Clients reconnect (possibly to another node) and RESUME. |
| Network partition | Writes buffered locally. Eventual convergence on partition heal. |

---

## 15. Schema Validation

Schema validation is OPTIONAL. When no schema file is loaded, all values
are accepted.

### 15.1 Schema File Format

A JSON file mapping state key names to type definitions:

```json
{
  "game.score": {
    "type": "Object",
    "fields": {
      "home": { "type": "Int" },
      "away": { "type": "Int" },
      "status": { "type": "String", "nullable": true }
    }
  },
  "user.profile": "Object"
}
```

### 15.2 Schema Node Types

**Simple form** — type name as a string:

```json
"user.profile": "Object"
```

**Full form** — type with field definitions:

```json
"game.score": {
  "type": "Object",
  "nullable": false,
  "fields": {
    "home": { "type": "Int" },
    "away": { "type": "Int" }
  }
}
```

### 15.3 Type System

| Type | Description |
|---|---|
| `String` | UTF-8 string. |
| `Int` | Integer value (MessagePack int). |
| `Float` | Floating-point value. Int values are also accepted (widening). |
| `Bool` | Boolean (`true` or `false`). |
| `Object` | Map with optionally typed fields. |
| `Array` | Ordered sequence of values. |
| `Any` | No type restriction. |

### 15.4 Validation Rules

1. Validation runs BEFORE the mutation is applied. Rejected values MUST
   NOT enter the state store.
2. Undeclared fields (present in the value but not in the schema) are
   permitted (permissive mode).
3. State keys not listed in the schema file accept all values.
4. Fields marked `nullable: true` accept null.
5. Float fields accept Int values (type widening).
6. Validation failure MUST return ERROR 422 with a descriptive message.

---

## 16. Security Considerations

### 16.1 Transport Security

- Production deployments MUST use TLS (`wss://`).
- Implementations SHOULD support TLS 1.2 or later.
- Self-signed certificates MUST NOT be used in production.

### 16.2 WebSocket-Specific

- Servers MUST validate the `Origin` header on WebSocket upgrade to
  prevent cross-site WebSocket hijacking (CSWSH).
- Servers SHOULD implement CORS-equivalent origin checking.
- Clients connecting from browsers are subject to the browser's
  same-origin policy for the initial HTTP upgrade request.

### 16.3 Authentication

- JWT tokens SHOULD have short expiration times (RECOMMENDED: 15 minutes
  to 1 hour).
- Clients SHOULD use a `tokenProvider` that obtains fresh tokens on each
  reconnect rather than reusing a stale token.
- Tokens MUST NOT be transmitted over unencrypted connections.
- Servers MUST NOT log token values at INFO level or higher.

### 16.4 Authorization

- ACL rules MUST be evaluated on every WATCH, RESUME, and CALL that
  references a state key.
- ACL denials MUST NOT reveal whether the key exists — the error message
  SHOULD be generic ("access denied"), not "key not found vs. forbidden."

### 16.5 Denial of Service

- **Subscription flooding:** Rate limiting on WATCH operations
  (Section 12.2) mitigates subscription-based DoS.
- **Write flooding:** Rate limiting on CALL operations mitigates
  write-based DoS.
- **Slow client exhaustion:** Backpressure eviction (Section 12.3)
  prevents a single slow client from consuming unbounded server memory.
- **Key-space exhaustion:** Implementations SHOULD support a configurable
  maximum number of state keys. Exceeding the limit SHOULD return
  ERROR 400.
- **Reconnect storms:** Clients MUST implement jittered exponential
  backoff (Section 9.6) to avoid thundering herd on server restart.

### 16.6 Data Integrity

- Mutations are applied atomically. Partial writes are not observable
  by subscribers.
- The server is the single source of truth. Clients MUST NOT modify
  their local cache except by applying server-delivered deltas.

---

## 17. IANA Considerations

### 17.1 URI Scheme Registration

This specification anticipates the registration of two URI schemes:

| Scheme | Description |
|---|---|
| `sodp` | SODP over an unencrypted transport (development, internal networks). |
| `sodps` | SODP over a TLS-encrypted transport (production). |

The default port for `sodp` and `sodps` is **7777**.

URI syntax:

```
sodp://host[:port]
sodps://host[:port]
```

Formal IANA registration will be submitted when the protocol reaches
sufficient adoption and a stable specification version.

### 17.2 Frame Type Registry

The frame type ID space (`0x00` - `0xFF`) is managed by this
specification. IDs `0x01` - `0x0D` are assigned (Section 5.1). IDs
`0x0E` - `0xFF` are reserved for future standardized frame types.

Implementations MUST NOT use reserved frame type IDs for
implementation-specific purposes. Extension frames, if needed in the
future, will be allocated from this reserved space.

### 17.3 MessagePack Format Identifier

SODP uses the MessagePack binary serialization format as specified at
https://github.com/msgpack/msgpack/blob/master/spec.md (2013 revision).
No IANA registration is required for MessagePack itself; this section
records the dependency for implementors.

---

## 18. Compliance Checklist

An implementation claiming SODP v1.0 compliance MUST satisfy all items
marked MUST in this specification. The following checklist summarizes the
key requirements.

### 18.1 Transport

- [ ] Sends and receives binary WebSocket messages.
- [ ] Sets `TCP_NODELAY` on the underlying socket.

### 18.2 Frame Format

- [ ] Encodes frames as 4-element MessagePack arrays.
- [ ] Uses the most compact integer encoding.
- [ ] Maintains monotonically increasing `seq` per direction.

### 18.3 Connection Lifecycle

- [ ] Server sends HELLO immediately on connection.
- [ ] Client waits for HELLO before sending frames (except HEARTBEAT).
- [ ] Server enforces AUTH before live session when `auth: true`.
- [ ] Server sends AUTH_OK on successful authentication.
- [ ] Server sends ERROR 401 and closes on failed authentication.

### 18.4 State Synchronization

- [ ] Server sends STATE_INIT in response to WATCH.
- [ ] Server sends DELTA frames for all mutations to watched keys.
- [ ] STATE_INIT includes `state`, `version`, `value`, `initialized`.
- [ ] DELTA includes `version` and `ops`.
- [ ] Multi-key WATCH decomposes into individual subscriptions.
- [ ] UNWATCH stops DELTA delivery for the specified key.

### 18.5 Delta Operations

- [ ] Supports ADD, UPDATE, REMOVE ops.
- [ ] Uses JSON Pointer (RFC 6901) paths.
- [ ] Supports `/` (root) path.
- [ ] Supports `/-` (array append) path.
- [ ] Supports numeric index paths on arrays.
- [ ] Rejects unknown op types (MUST NOT silently ignore).
- [ ] Applies ops in order within a single DELTA frame.
- [ ] Does not mutate input state (immutable application).
- [ ] Initializes arrays (not objects) when path implies array context.

### 18.6 RPC

- [ ] Supports CALL with `call_id`, `method`, `args`.
- [ ] Responds with exactly one RESULT per CALL.
- [ ] Implements `state.set`, `state.patch`, `state.set_in`, `state.delete`.
- [ ] Implements `state.presence` with session-lifetime binding.
- [ ] Returns ERROR 404 for unknown methods.

### 18.7 RESUME

- [ ] Replays stored deltas with version > `since_version`.
- [ ] Sends STATE_INIT as the final "live" marker after replay.
- [ ] Falls back to STATE_INIT-only when delta log is insufficient.

### 18.8 Heartbeat

- [ ] Server sends HEARTBEAT at a regular interval.
- [ ] Client echoes HEARTBEAT back.
- [ ] Server closes connection on heartbeat timeout.

### 18.9 Error Handling

- [ ] Uses the error codes defined in Section 11.1.
- [ ] Closes connection on ERROR 401.
- [ ] Keeps connection open on all other error codes.
- [ ] Never silently discards errors.

### 18.10 Security (when authentication is enabled)

- [ ] Validates JWT signature (HS256 or RS256).
- [ ] Rejects expired tokens.
- [ ] Enforces token expiry live during active sessions.
- [ ] Evaluates ACL rules on every key access.

---

## Appendix A. Frame Type Quick Reference

```
0x01  HELLO       S->C   { protocol, version, server?, auth, capabilities? }
0x02  WATCH       C->S   { state, params? } | { states, params? }
0x03  STATE_INIT  S->C   { state, version, value, initialized, params? }
0x04  DELTA       S->C   { version, ops, state? }
0x05  CALL        C->S   { call_id, method, args }
0x06  RESULT      S->C   { call_id, success, data }
0x07  ERROR       S->C   { code, message }
0x08  (reserved)
0x09  HEARTBEAT   Both   null
0x0A  RESUME      C->S   { state, since_version, params? }
0x0B  AUTH        C->S   { token }
0x0C  AUTH_OK     S->C   { sub }
0x0D  UNWATCH     C->S   { state } | { states }
```

## Appendix B. Error Code Quick Reference

```
400  Bad Request        Malformed frame or missing required field.
401  Unauthorized       Invalid, expired, or missing JWT. Connection closed.
403  Forbidden          ACL denies access.
404  Not Found          Unknown method in CALL.
422  Unprocessable      Schema validation failed.
429  Rate Limited       Per-session rate limit exceeded.
500  Internal Error     Unexpected server error.
```

## Appendix C. Environment Variables (Reference Implementation)

| Variable | Purpose | Default |
|---|---|---|
| `SODP_JWT_SECRET` | HS256 shared secret | *(auth disabled)* |
| `SODP_JWT_PUBLIC_KEY_FILE` | RS256 public key PEM path | *(auth disabled)* |
| `SODP_ACL_FILE` | ACL rules JSON path | *(all access allowed)* |
| `SODP_HEALTH_PORT` | Health endpoint port | *(disabled)* |
| `SODP_METRICS_PORT` | Prometheus metrics port | *(disabled)* |
| `SODP_RATE_WRITES_PER_SEC` | Write rate limit per session | *(no limit)* |
| `SODP_RATE_WATCHES_PER_SEC` | Watch rate limit per session | *(no limit)* |
| `SODP_BACKPRESSURE_LIMIT` | Bounded channel capacity per session | `1024` |
| `SODP_REDIS_URL` | Redis URL for horizontal scaling | *(single-node mode)* |

## Appendix D. Delta Encoding Examples

### D.1 Field Update

State before: `{ "name": "Alice", "health": 100, "position": { "x": 0, "y": 0 } }`

Mutation: `state.patch { "health": 80, "position": { "x": 5 } }`

```json
{
  "version": 7,
  "ops": [
    { "op": "UPDATE", "path": "/health", "value": 80 },
    { "op": "UPDATE", "path": "/position/x", "value": 5 }
  ]
}
```

Only the two changed fields are transmitted. `name` and `position.y` are
unchanged and produce no ops.

### D.2 Key Deletion

Mutation: `state.delete "game.player"`

```json
{
  "version": 8,
  "ops": [
    { "op": "REMOVE", "path": "/" }
  ]
}
```

### D.3 Array Append

State before: `[1, 2, 3]`

Server emits:

```json
{
  "version": 9,
  "ops": [
    { "op": "ADD", "path": "/-", "value": 4 }
  ]
}
```

State after: `[1, 2, 3, 4]`

### D.4 First Write to an Uninitialized Key

State before: `null` (`initialized: false`)

Mutation: `state.set { "name": "Alice" }`

```json
{
  "version": 10,
  "ops": [
    { "op": "ADD", "path": "/", "value": { "name": "Alice" } }
  ]
}
```

---

*This specification consolidates and supersedes all previous SODP
documentation (docs/ref_v1.md through docs/ref_v6.md and
docs/protocol.md). Those documents are retained for historical reference
only.*
