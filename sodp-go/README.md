# sodp-go

Go library implementing the [State-Oriented Data Protocol (SODP)](https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md) — a WebSocket protocol for real-time state synchronization.

Clients subscribe to named state keys and receive a full snapshot followed by incremental deltas containing only the changed fields. One mutation to a 100-field object sends only the changed fields over the wire.

## Install

```sh
go get github.com/orkestri/sodp-go
```

## Quickstart

```go
package main

import (
    "net/http"
    sodp "github.com/orkestri/sodp-go"
)

func main() {
    srv := sodp.NewServer()

    // Push state from your application at any time.
    go func() {
        srv.Mutate("game.state", map[string]any{"score": 0, "players": 0})
    }()

    http.HandleFunc("/sodp", srv.HandleWS)
    http.ListenAndServe(":7777", nil)
}
```

Any client using `@sodp/client` (TypeScript), `sodp` (Python), or `io.sodp:sodp-client` (Java) can connect to `sodp://localhost:7777`.

## API

### Creating a server

```go
srv := sodp.NewServer(
    sodp.WithJWTSecret([]byte("my-secret")),         // HS256 auth
    sodp.WithJWTPublicKey(rsaPublicKeyPEM),           // RS256 auth (takes priority)
    sodp.WithAllowedOrigins([]string{"https://app.example.com"}),
    sodp.WithMaxSessions(1000),
    sodp.WithRateLimit(50),       // CALL mutations per session per second
    sodp.WithMaxWatches(32),      // concurrent WATCH subscriptions per session
    sodp.WithBackpressureLimit(256), // outbound buffer per session
    sodp.WithAuthorizeKey(myAuthFn), // per-key access control
)

http.HandleFunc("/sodp", srv.HandleWS)
```

### Server-side mutations

```go
// Replace full value.
srv.Mutate("leaderboard", map[string]any{"rank": []any{"alice", "bob"}})

// Append to a slice (O(1) delta — only the new element is sent).
srv.MutateAppend("chat.messages", map[string]any{"user": "alice", "text": "hi"}, 500)

// Delete a key.
srv.MutateDelete("session.xyz")
```

### Per-key authorization

```go
srv := sodp.NewServer(
    sodp.WithJWTSecret(secret),
    sodp.WithAuthorizeKey(func(sess *sodp.Session, key string) (bool, int, string) {
        // sess.Sub is the JWT "sub" claim; sess.Claims has all claims.
        orgID, _ := sess.Claims["org_id"].(string)
        if strings.HasPrefix(key, "org."+orgID+".") {
            return true, 0, ""
        }
        return false, 403, "access denied"
    }),
)
```

### State eviction

`StateStore.EvictIf` lets you clean up stale state with a predicate:

```go
// Evict runs older than 1 hour.
srv.State.EvictIf(func(key string, value any) bool {
    m, ok := value.(map[string]any)
    if !ok { return false }
    if m["status"] != "done" { return false }
    t, _ := time.Parse(time.RFC3339, m["finished_at"].(string))
    return time.Since(t) > time.Hour
})
```

### Reading state server-side

```go
val, version := srv.State.Get("config")
allKeys := srv.State.Keys()
snapshot := srv.State.Snapshot("org.acme") // all keys with prefix "org.acme"
```

## Wire format

SODP frames are 4-element MessagePack arrays: `[frame_type: u8, stream_id: u32, seq: u64, body: any]`.

| Frame type | Value | Direction |
|------------|-------|-----------|
| HELLO      | 0x01  | server→client |
| WATCH      | 0x02  | client→server |
| STATE_INIT | 0x03  | server→client |
| DELTA      | 0x04  | server→client |
| CALL       | 0x05  | client→server |
| RESULT     | 0x06  | server→client |
| ERROR      | 0x07  | server→client |
| HEARTBEAT  | 0x09  | bidirectional |
| RESUME     | 0x0A  | client→server |
| AUTH       | 0x0B  | client→server |
| AUTH_OK    | 0x0C  | server→client |
| UNWATCH    | 0x0D  | client→server |

See the [full specification](https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md).

## Client methods supported

Clients using `state.set`, `state.patch`, `state.set_in`, `state.delete`, and `state.append` CALL methods are all handled. Multi-key WATCH (`states` array), RESUME (delta-log replay), and HEARTBEAT echo are all implemented.

## Tests

```sh
go test ./...
```

44 tests covering frame codec, delta diff, state store, fanout, session lifecycle, and full WebSocket integration including RESUME and connection-limit behavior.

## License

MIT
