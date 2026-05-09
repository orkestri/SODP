# IANA URI Scheme Registration — `sodp` and `sodps`

Provisional registration per [RFC 7595, Section 3.8](https://www.rfc-editor.org/rfc/rfc7595#section-3.8).

---

## Registration Template: `sodp`

**Scheme name:** sodp

**Status:** Provisional

**Applications/protocols that use this scheme name:**
State-Oriented Data Protocol (SODP) — a binary protocol for continuous
state synchronization over persistent bidirectional connections. SODP
replaces request-response patterns with a subscribe-and-stream model
where clients receive full snapshots followed by incremental deltas
containing only changed fields. The protocol uses MessagePack-encoded
frames over WebSocket (RFC 6455) as its reference transport binding.

**Contact:**
Orkestri
hello@orkestri.site

**Change controller:**
Orkestri
hello@orkestri.site

**References:**
SODP Protocol Specification v1.0
https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md

---

## Registration Template: `sodps`

**Scheme name:** sodps

**Status:** Provisional

**Applications/protocols that use this scheme name:**
State-Oriented Data Protocol (SODP) over TLS — the TLS-secured variant
of the SODP protocol. Semantically identical to `sodp` but requires the
underlying transport connection to be encrypted using TLS 1.2 or later.
Analogous to the relationship between `ws` and `wss` (RFC 6455), or
`mqtt` and `mqtts`.

**Contact:**
Orkestri
hello@orkestri.site

**Change controller:**
Orkestri
hello@orkestri.site

**References:**
SODP Protocol Specification v1.0
https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md

---

## URI Syntax

```
sodp-URI  = "sodp://" authority [ "/" ]
sodps-URI = "sodps://" authority [ "/" ]
authority = host [ ":" port ]
```

**Default port:** 7777

When no port is specified, implementations MUST default to port 7777.

**Examples:**

```
sodp://localhost:7777
sodp://sodp.example.com
sodps://sodp.example.com
sodps://sodp.example.com:8443
```

---

## Specification Summary

SODP defines a frame-based protocol for real-time state synchronization:

1. **Transport:** Persistent bidirectional connection (reference binding: WebSocket).
2. **Encoding:** MessagePack binary (4-element array: `[frame_type, stream_id, seq, body]`).
3. **Lifecycle:** Client subscribes to named state keys via WATCH. Server delivers a full snapshot (STATE_INIT) followed by incremental deltas (DELTA) containing only changed fields as JSON Pointer-addressed operations (ADD, UPDATE, REMOVE).
4. **RPC:** Integrated mutation layer via CALL/RESULT frames with built-in methods for state manipulation.
5. **Reconnection:** RESUME frame replays missed deltas by version, providing seamless recovery after disconnects.
6. **Authentication:** Optional JWT-based authentication (HS256/RS256) with per-key ACL authorization.

### Frame Type Registry

| ID   | Name       | Direction       |
|------|------------|-----------------|
| 0x01 | HELLO      | server->client  |
| 0x02 | WATCH      | client->server  |
| 0x03 | STATE_INIT | server->client  |
| 0x04 | DELTA      | server->client  |
| 0x05 | CALL       | client->server  |
| 0x06 | RESULT     | server->client  |
| 0x07 | ERROR      | server->client  |
| 0x08 | (reserved) | —               |
| 0x09 | HEARTBEAT  | bidirectional   |
| 0x0A | RESUME     | client->server  |
| 0x0B | AUTH       | client->server  |
| 0x0C | AUTH_OK    | server->client  |
| 0x0D | UNWATCH    | client->server  |

IDs 0x0E–0xFF are reserved for future use.

### Implementations

| Component | Language | Package |
|-----------|----------|---------|
| Reference server | Rust | [github.com/orkestri/SODP](https://github.com/orkestri/SODP) |
| Client SDK | TypeScript | [@sodp/client](https://www.npmjs.com/package/@sodp/client) |
| React hooks | TypeScript | [@sodp/react](https://www.npmjs.com/package/@sodp/react) |
| Client SDK | Python | [sodp](https://pypi.org/project/sodp/) |
| Client SDK | Java | io.sodp:sodp-client |

### Security Considerations

See Section 16 of the full specification. Key points:

- The `sodps` scheme REQUIRES TLS 1.2 or later.
- The `sodp` scheme (unencrypted) is intended for development and trusted
  internal networks only. Production deployments SHOULD use `sodps`.
- JWT-based authentication with live expiry enforcement prevents session hijacking.
- Per-key ACL authorization prevents unauthorized state access.
- Per-session rate limiting and backpressure eviction mitigate denial-of-service attacks.
