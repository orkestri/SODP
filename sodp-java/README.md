# sodp-client — Java

[![Maven](https://img.shields.io/github/v/release/orkestri/SODP?label=maven)](https://github.com/orkestri/SODP/packages)
[![Java](https://img.shields.io/badge/java-17%2B-blue)](https://adoptium.net/)
[![license](https://img.shields.io/github/license/orkestri/SODP)](https://github.com/orkestri/SODP/blob/main/LICENSE)

Java 17+ client for the **State-Oriented Data Protocol (SODP)** — a WebSocket-based protocol for continuous state synchronization.

Instead of polling or request/response, SODP streams every change as a minimal delta to all connected subscribers. Framework-agnostic: works with Spring Boot, Quarkus, Micronaut, or any plain Java project.

→ [Protocol spec & server](https://github.com/orkestri/SODP)

---

## Install

### Maven

```xml
<dependency>
    <groupId>io.sodp</groupId>
    <artifactId>sodp-client</artifactId>
    <version>0.1.0</version>
</dependency>
```

### Gradle

```groovy
implementation 'io.sodp:sodp-client:0.1.0'
```

### GitHub Packages

The package is hosted on GitHub Packages. Add the repository to your build file and authenticate with a GitHub token:

**Maven** — `~/.m2/settings.xml`:

```xml
<settings>
  <servers>
    <server>
      <id>github-sodp</id>
      <username>YOUR_GITHUB_USERNAME</username>
      <password>YOUR_GITHUB_TOKEN</password>  <!-- token needs read:packages scope -->
    </server>
  </servers>
</settings>
```

`pom.xml`:

```xml
<repositories>
  <repository>
    <id>github-sodp</id>
    <url>https://maven.pkg.github.com/orkestri/SODP</url>
  </repository>
</repositories>
```

**Gradle** — `build.gradle`:

```groovy
repositories {
    maven {
        url = uri("https://maven.pkg.github.com/orkestri/SODP")
        credentials {
            username = project.findProperty("gpr.user") ?: System.getenv("GITHUB_ACTOR")
            password = project.findProperty("gpr.key")  ?: System.getenv("GITHUB_TOKEN")
        }
    }
}
```

---

## Quick start

```java
SodpClient client = SodpClient.builder("ws://localhost:7777").build();

// Wait for connection (blocks until authenticated or throws)
client.ready().get();

// Subscribe to a state key
Runnable unsub = client.watch("game.score", Integer.class, (value, meta) ->
    System.out.printf("score=%d  version=%d%n", value, meta.version()));

// Mutate state
client.set("game.score", 42).get();
client.patch("game.player", Map.of("health", 80)).get();

unsub.run();       // cancel this callback
client.close();    // close the connection
```

---

## Authentication

```java
// Static token
SodpClient client = SodpClient.builder(url)
    .token("eyJhbGci...")
    .build();

// Dynamic token provider — called on every connect/reconnect
SodpClient client = SodpClient.builder(url)
    .tokenProvider(() -> tokenService.getFreshToken())
    .build();
```

---

## API reference

### `SodpClient.builder(url)`

Fluent builder. Call `.build()` to create the client and start connecting immediately.

```java
SodpClient client = SodpClient.builder("ws://localhost:7777")
    .token("eyJ...")                        // static JWT
    .tokenProvider(() -> getToken())        // dynamic JWT; supersedes token()
    .autoReconnect(true)                    // default: true
    .onConnect(() -> log.info("connected"))
    .onDisconnect(() -> log.info("dropped"))
    .objectMapper(myMapper)                 // share framework's ObjectMapper
    .build();
```

| Builder method | Default | Description |
|---|---|---|
| `.token(String)` | — | Static JWT |
| `.tokenProvider(Supplier<String>)` | — | Called on every connect/reconnect |
| `.autoReconnect(boolean)` | `true` | Auto-reconnect with exponential backoff |
| `.onConnect(Runnable)` | — | Called when connected and authenticated |
| `.onDisconnect(Runnable)` | — | Called when connection drops |
| `.objectMapper(ObjectMapper)` | new instance | Share your framework's mapper |

---

### `client.ready(): CompletableFuture<Void>`

Completes once the client is connected and authenticated. Throws `SodpException` if the token is rejected (401).

```java
client.ready().get();                  // blocking
client.ready().thenRun(this::onReady); // non-blocking
```

---

### `client.watch(key, type, callback): Runnable`

Subscribe to a state key with a typed callback.

```java
record Player(String name, int health) {}

Runnable unsub = client.watch("game.player", Player.class, (player, meta) -> {
    if (!meta.initialized()) return;
    System.out.printf("%s  HP: %d  v%d%n", player.name(), player.health(), meta.version());
});

// Later:
unsub.run(); // remove this callback
```

For raw JSON access, omit the type argument:

```java
Runnable unsub = client.watch("game.player", (node, meta) ->
    System.out.println(node.toPrettyString()));
```

The callback receives:
- `value` — deserialized as `T`, or raw `JsonNode`
- `meta.version()` — monotonically increasing version (`long`)
- `meta.initialized()` — `false` when the key has never been written

Multiple `watch()` calls for the same key share a single server subscription.

---

### `client.state(key, type): StateRef<T>`

Returns a key-scoped handle for cleaner per-key code:

```java
record Player(String name, int health, Position position) {}
record Position(double x, double y) {}

StateRef<Player> player = client.state("game.player", Player.class);

Runnable unsub = player.watch((value, meta) -> System.out.println(value.name()));

player.set(new Player("Alice", 100, new Position(0, 0))).get();
player.patch(Map.of("health", 80)).get();
player.setIn("/position/x", 5.0).get();
player.delete().get();
player.presence("/alice", Map.of("name", "Alice", "line", 1)).get();

Optional<Player> snapshot = player.getSnapshot();
player.unwatch();
```

---

### Write methods

All write methods return `CompletableFuture<JsonNode>`.

```java
// Replace full value
client.set("game.score", 42).get();
client.set("game.player", new Player("Alice", 100)).get();  // POJO → JSON

// Deep-merge partial update (only changed fields sent to watchers)
client.patch("game.player", Map.of("health", 80)).get();

// Atomically set a nested field by JSON Pointer
client.setIn("game.player", "/position/x", 5.0).get();

// Remove a key entirely
client.delete("game.player").get();

// Session-lifetime path (auto-removed on disconnect)
client.presence("collab.cursors", "/alice", Map.of("name", "Alice", "line", 1)).get();
```

---

### `client.call(method, args): CompletableFuture<JsonNode>`

Low-level method invocation for custom server methods or direct protocol access:

```java
client.call("state.set", Map.of(
    "state", "game.score",
    "value", 42
)).get();
```

---

### `client.getSnapshot(key, type): Optional<T>`

Synchronously read the cached value without subscribing. Returns `Optional.empty()` if the key is not being watched or no value has arrived yet.

```java
Optional<Player> player = client.getSnapshot("game.player", Player.class);
```

---

### `client.unwatch(key)`

Cancel the server subscription and clear all local state for a key. Removes all registered callbacks.

---

### `client.isWatching(key): boolean`

Returns `true` if there is at least one active callback registered for `key`.

---

### `client.close()`

Gracefully close the WebSocket and stop all background threads.

---

## Presence

Presence binds a nested path to the session lifetime. The server automatically removes it and notifies all watchers when the client disconnects — no ghost cursors or stale "online" flags:

```java
// Register this instance's status — auto-removed on JVM shutdown or network drop
client.presence("services.instances", "/instance-1",
    Map.of("host", hostname, "started", Instant.now().toString())
).get();
```

---

## Spring Boot example

```java
@Configuration
public class SodpConfig {

    @Bean
    public SodpClient sodpClient(
            @Value("${sodp.url}") String url,
            @Value("${sodp.token}") String token,
            ObjectMapper objectMapper
    ) {
        return SodpClient.builder(url)
                .token(token)
                .objectMapper(objectMapper)
                .build();
    }
}

@RestController
@RequiredArgsConstructor
public class ScoreController {

    private final SodpClient sodp;

    @PostMapping("/score/{value}")
    public CompletableFuture<Void> setScore(@PathVariable int value) {
        return sodp.set("game.score", value).thenApply(ignored -> null);
    }
}
```

---

## StateRef API summary

| Method | Description |
|---|---|
| `ref.watch(cb)` | Subscribe; returns unsub `Runnable` |
| `ref.getSnapshot()` | `Optional<T>` cached value |
| `ref.isWatching()` | `true` if subscribed |
| `ref.unwatch()` | Cancel subscription + clear local state |
| `ref.set(value)` | Replace full value |
| `ref.patch(partial)` | Deep-merge partial update |
| `ref.setIn(path, value)` | Set nested field by JSON Pointer |
| `ref.delete()` | Remove key from server |
| `ref.presence(path, value)` | Session-lifetime path binding |

---

## Requirements

- Java 17 or later
- No additional dependencies beyond Jackson (`com.fasterxml.jackson.core`) and `org.msgpack:msgpack-core`
