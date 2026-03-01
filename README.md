# SODP — State-Oriented Data Protocol

A lightweight, high-performance protocol for **continuous state synchronization** over WebSocket.

Instead of polling or request-response, clients subscribe to named state keys and receive a snapshot followed by a stream of structural diffs (deltas) as the state changes. The result is a persistent replica — always current, always minimal on the wire.

```
Client                          Server
  │── WATCH "game.player" ────▶ │
  │◀─ STATE_INIT { health:100 } │
  │                             │  (another client writes health: 80)
  │◀─ DELTA { /health → 80 }   │
  │◀─ DELTA { /health → 60 }   │
```

---

## Why not REST / SSE / GraphQL?

| | REST+polling | SSE | gRPC stream | **SODP** |
|---|---|---|---|---|
| Full object on every update | yes | yes | yes | **no — delta only** |
| Client drives timing | yes | no | no | no |
| Native browser support | yes | yes | no | **yes (WebSocket)** |
| Reconnect with replay | manual | manual | manual | **built-in** |
| Bytes per update (11-field obj) | 353 | 353 | 363 | **54** |
| P50 latency (localhost) | 155 µs | 155 µs | 278 µs | **114 µs** |

*Benchmark: 2 000 iterations, release build, localhost — see `src/bin/bench.rs`.*

---

## Features

- **WATCH / DELTA stream** — subscribe once, receive only what changed
- **CALL methods** — `state.set`, `state.patch`, `state.set_in`, `state.delete`, `state.presence`
- **RESUME** — on reconnect the server replays missed deltas; clients never need to re-fetch
- **Persistence** — segmented append log, crash-safe, auto-compacted
- **JWT authentication** — HS256 (shared secret) or RS256 (public key); token providers for rotation
- **Per-key ACL** — dot-pattern rules, `{sub}` capture, claim-aware permissions; built-in IdP presets
- **Rate limiting** — per-session write and watch limits; `ERROR 429` without closing the connection
- **Schema validation** — SDL enforced server-side before every write; `ERROR 422` on violation
- **Presence** — session-scoped path bindings auto-removed on disconnect (no ghost entries)
- **Health check** — HTTP endpoint for load-balancer probes
- **Prometheus metrics** — mutation latency, fanout size, connection count, rate-limited frames
- **Graceful shutdown** — `SIGTERM` / Ctrl-C drains sessions cleanly

---

## Quick start

### Server

```bash
# Build
cargo build --release

# Ephemeral (in-memory only)
./target/release/sodp-server 0.0.0.0:7777

# With persistence
./target/release/sodp-server 0.0.0.0:7777 /var/lib/sodp/log

# With persistence + schema validation
./target/release/sodp-server 0.0.0.0:7777 /var/lib/sodp/log /etc/sodp/schema.json

# Persistence disabled, schema only (pass empty string for log dir)
./target/release/sodp-server 0.0.0.0:7777 "" /etc/sodp/schema.json
```

### TypeScript client

```bash
cd client-ts && npm install && npm run build
```

```typescript
import { SodpClient } from "@sodp/client";

const client = new SodpClient("ws://localhost:7777");
await client.ready;

// Subscribe
const unsub = client.watch<{ score: number }>("game.score", (value, meta) => {
  console.log("score:", value?.score, "v", meta.version);
});

// Mutate
await client.set("game.score", { score: 42 });

// Cleanup
unsub();
client.close();
```

### Python client

```bash
pip install sodp-py   # or: pip install -e sodp-py/
```

```python
import asyncio
from sodp.client import SodpClient

async def main():
    client = SodpClient("ws://localhost:7777")
    await client.ready

    def on_update(value, meta):
        print("score:", value, "v", meta.version)

    client.watch("game.score", on_update)
    await client.set("game.score", {"score": 42})
    await asyncio.sleep(1)
    client.close()

asyncio.run(main())
```

---

## Documentation

| Document | Contents |
|---|---|
| [docs/tutorial.md](docs/tutorial.md) | Step-by-step: from zero to a production server with auth, ACL, schema |
| [docs/guide.md](docs/guide.md) | Full API reference — state methods, JWT, ACL presets, rate limiting, schema SDL |
| [docs/protocol.md](docs/protocol.md) | Wire protocol — frame format, connection lifecycle, body schemas |
| [docs/architecture.md](docs/architecture.md) | Server internals — state store, delta engine, fanout, persistence |
| [docs/deployment.md](docs/deployment.md) | TLS termination, nginx/Caddy, systemd, Docker, env-var reference |
| [docs/clustering.md](docs/clustering.md) | Horizontal scaling — Redis state sync, cross-node fanout, failure modes |
| [docs/ref_v1.md … ref_v6.md](docs/) | Original protocol design specification v0.1 – v0.6 |

---

## Repository layout

```
src/
  main.rs      — server binary, env-var wiring, health/metrics tasks
  server.rs    — SodpServer: connection handling, auth, ACL, rate limits, frame dispatch
  session.rs   — per-connection state: watches, presence, rate limiters, JWT claims
  state.rs     — StateStore (DashMap) + DeltaLog (capped ring) + SegmentedLog
  fanout.rs    — FanoutBus: broadcast encoded deltas to all watchers
  frame.rs     — Frame struct, MessagePack codec, frame constructors
  delta.rs     — structural diff: DeltaOp (ADD / UPDATE / REMOVE), diff()
  acl.rs       — AclRegistry: rule matching, claim resolution, IdP presets
  schema.rs    — SchemaRegistry: SDL validation, type widening, nullable fields
  log.rs       — SegmentedLog: crash-safe append log, compaction
  lib.rs       — module re-exports
client-ts/     — TypeScript client library (@sodp/client)
sodp-py/       — Python client library (sodp-py)
demo-collab/   — collaborative editor demo (presence, cursors)
docs/          — protocol spec + deployment guide
```

---

## License

MIT
