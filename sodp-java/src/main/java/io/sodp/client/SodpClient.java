package io.sodp.client;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.sodp.client.delta.DeltaApplier;
import io.sodp.client.delta.DeltaOp;
import io.sodp.client.internal.CallbackEntry;
import io.sodp.client.internal.FrameTypes;
import io.sodp.client.internal.Frames;
import io.sodp.client.internal.WatchEntry;

import java.io.ByteArrayOutputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.WebSocket;
import java.nio.ByteBuffer;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.Supplier;
import java.util.logging.Logger;

/**
 * SODP WebSocket client for Java 17+.
 *
 * <p>Framework-agnostic: works with Spring Boot, Quarkus, Micronaut, or any
 * Java project.  Use the {@link Builder} to construct an instance:
 *
 * <pre>{@code
 * SodpClient client = SodpClient.builder("ws://localhost:7777")
 *     .token("eyJ...")          // optional JWT
 *     .objectMapper(mapper)     // share framework's ObjectMapper
 *     .build();
 *
 * client.ready().get();         // wait for auth
 *
 * Runnable unsub = client.watch("game.score", Integer.class, (value, meta) ->
 *     System.out.println("score=" + value + " v" + meta.version()));
 *
 * client.set("game.score", 42).get();
 *
 * client.close();
 * }</pre>
 */
public final class SodpClient {

    private static final Logger LOG = Logger.getLogger(SodpClient.class.getName());

    // ── Configuration ─────────────────────────────────────────────────────────

    private final String              url;
    private final boolean             autoReconnect;
    private final String              staticToken;
    private final Supplier<String>    tokenProvider;
    private final Runnable            onConnect;
    private final Runnable            onDisconnect;
    private final ObjectMapper        mapper;

    // ── Runtime state ─────────────────────────────────────────────────────────

    private final HttpClient          httpClient = HttpClient.newHttpClient();

    /** The active WebSocket; null while disconnected. */
    private volatile WebSocket        ws;

    private volatile boolean          authenticated = false;
    private volatile boolean          closed        = false;

    /** Completes when first authenticated (or on HELLO with auth:false). */
    private final CompletableFuture<Void> readyFuture = new CompletableFuture<>();

    // ── Watch registry ────────────────────────────────────────────────────────

    /** state key → per-key entry */
    private final ConcurrentHashMap<String, WatchEntry> watches      = new ConcurrentHashMap<>();
    /** stream_id → state key (for DELTA routing) */
    private final ConcurrentHashMap<Integer, String>    streamToKey  = new ConcurrentHashMap<>();
    /** Monotonically increasing stream IDs; starts at 10 (protocol requirement). */
    private final AtomicInteger                         nextStreamId = new AtomicInteger(10);

    // ── Call registry ─────────────────────────────────────────────────────────

    /** call_id → pending CompletableFuture */
    private final ConcurrentHashMap<String, CompletableFuture<JsonNode>> pendingCalls =
            new ConcurrentHashMap<>();

    // ── Send infrastructure ───────────────────────────────────────────────────

    /**
     * Frames queued before authentication completes.  Flushed in
     * {@link #becomeAuthenticated()}.
     */
    private final List<byte[]>         sendQueue   = Collections.synchronizedList(new ArrayList<>());

    /**
     * Sequential outbound queue.  {@code java.net.http.WebSocket} requires that
     * the next {@code sendBinary} is called only after the previous one has
     * completed — the dedicated sender thread guarantees this.
     */
    private final LinkedBlockingQueue<byte[]> outbound = new LinkedBlockingQueue<>();
    private final Thread                      senderThread;

    // ── Scheduling ────────────────────────────────────────────────────────────

    /** Used for call timeouts and reconnect delays. */
    private final ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor(r -> {
        Thread t = new Thread(r, "sodp-scheduler");
        t.setDaemon(true);
        return t;
    });

    /** Milliseconds before the next reconnect attempt (doubles up to 30 s). */
    private long reconnectDelayMs = 1_000;

    private final AtomicLong seqCounter = new AtomicLong(0);

    // ── Construction ──────────────────────────────────────────────────────────

    private SodpClient(Builder b) {
        this.url           = b.url;
        this.autoReconnect = b.autoReconnect;
        this.staticToken   = b.token;
        this.tokenProvider = b.tokenProvider;
        this.onConnect     = b.onConnect;
        this.onDisconnect  = b.onDisconnect;
        this.mapper        = b.objectMapper != null ? b.objectMapper : new ObjectMapper();

        // Dedicated sender thread — sequential sends satisfy java.net.http.WebSocket constraint.
        senderThread = new Thread(() -> {
            while (!Thread.currentThread().isInterrupted()) {
                try {
                    byte[] frame = outbound.take();
                    WebSocket w  = ws;
                    if (w != null) {
                        try {
                            w.sendBinary(ByteBuffer.wrap(frame), true).get();
                        } catch (ExecutionException e) {
                            LOG.fine("Send failed (connection dropped): " + e.getCause());
                        }
                    }
                    // If ws is null (disconnected), frame is silently discarded.
                    // WATCH/RESUME are re-sent by becomeAuthenticated() on reconnect.
                    // CALL futures will timeout after 30 s.
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    break;
                } catch (Exception e) {
                    LOG.fine("Sender error: " + e.getMessage());
                }
            }
        }, "sodp-sender");
        senderThread.setDaemon(true);
        senderThread.start();

        connect();
    }

    /** Start building a {@link SodpClient}. */
    public static Builder builder(String url) {
        return new Builder(url);
    }

    // ── Connection management ─────────────────────────────────────────────────

    private void connect() {
        if (closed) return;
        URI uri;
        try {
            uri = URI.create(url);
        } catch (IllegalArgumentException e) {
            readyFuture.completeExceptionally(new SodpException("Invalid URL: " + url));
            return;
        }
        httpClient.newWebSocketBuilder()
                  .buildAsync(uri, new WsListener())
                  .exceptionally(ex -> { handleDisconnect(); return null; });
    }

    private void handleDisconnect() {
        authenticated = false;
        ws = null;

        // Reset stream IDs so becomeAuthenticated() will re-subscribe all keys.
        for (WatchEntry entry : watches.values()) entry.streamId = 0;
        streamToKey.clear();

        if (!closed && onDisconnect != null) {
            try { onDisconnect.run(); } catch (Exception e) { LOG.fine("onDisconnect threw: " + e); }
        }

        if (!closed && autoReconnect) {
            long delay      = reconnectDelayMs;
            reconnectDelayMs = Math.min(reconnectDelayMs * 2, 30_000);
            scheduler.schedule(this::connect, delay, TimeUnit.MILLISECONDS);
        }
    }

    private void becomeAuthenticated() {
        reconnectDelayMs = 1_000; // reset backoff on success
        authenticated    = true;
        readyFuture.complete(null);

        if (onConnect != null) {
            try { onConnect.run(); } catch (Exception e) { LOG.fine("onConnect threw: " + e); }
        }

        // Re-subscribe all watched keys (RESUME if we have a version, WATCH otherwise).
        for (Map.Entry<String, WatchEntry> e : watches.entrySet()) {
            if (e.getValue().streamId == 0) {
                subscribeOrResume(e.getKey(), e.getValue());
            }
        }

        // Flush frames that were queued before auth completed.
        synchronized (sendQueue) {
            for (byte[] frame : sendQueue) enqueue(frame);
            sendQueue.clear();
        }
    }

    private void subscribeOrResume(String key, WatchEntry entry) {
        int streamId = nextStreamId.getAndIncrement();
        entry.streamId = streamId;
        streamToKey.put(streamId, key);

        byte[] frame = entry.version > 0
                ? Frames.encode(FrameTypes.RESUME, streamId, nextSeq(),
                        mapOf("state", key, "since_version", entry.version))
                : Frames.encode(FrameTypes.WATCH, streamId, nextSeq(),
                        mapOf("state", key));
        enqueue(frame);
    }

    // ── WebSocket listener ────────────────────────────────────────────────────

    private class WsListener implements WebSocket.Listener {

        /** Accumulates binary message fragments until last == true. */
        private final ByteArrayOutputStream accumulator = new ByteArrayOutputStream();

        @Override
        public void onOpen(WebSocket webSocket) {
            ws = webSocket;
            webSocket.request(1);
        }

        @Override
        public CompletableFuture<?> onBinary(WebSocket webSocket, ByteBuffer data, boolean last) {
            byte[] chunk = new byte[data.remaining()];
            data.get(chunk);
            accumulator.write(chunk, 0, chunk.length);

            if (last) {
                byte[] raw = accumulator.toByteArray();
                accumulator.reset();
                try {
                    handleFrame(raw);
                } catch (Exception e) {
                    LOG.warning("Error handling frame: " + e.getMessage());
                }
            }
            webSocket.request(1);
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public CompletableFuture<?> onClose(WebSocket webSocket, int statusCode, String reason) {
            handleDisconnect();
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public void onError(WebSocket webSocket, Throwable error) {
            LOG.fine("WebSocket error: " + error.getMessage());
            handleDisconnect();
        }
    }

    // ── Frame dispatch ────────────────────────────────────────────────────────

    private void handleFrame(byte[] bytes) {
        Frames.DecodedFrame f = Frames.decode(bytes);
        switch (f.frameType()) {
            case FrameTypes.HELLO      -> onHello(f);
            case FrameTypes.AUTH_OK    -> becomeAuthenticated();
            case FrameTypes.STATE_INIT -> onStateInit(f);
            case FrameTypes.DELTA      -> onDelta(f);
            case FrameTypes.RESULT     -> onResult(f);
            case FrameTypes.ERROR      -> onError(f);
            case FrameTypes.HEARTBEAT  -> enqueue(Frames.encodeNull(FrameTypes.HEARTBEAT, 0, 0));
            default -> { /* unknown frame types ignored */ }
        }
    }

    private void onHello(Frames.DecodedFrame f) {
        boolean authRequired = f.body() != null && f.body().path("auth").asBoolean(false);
        if (authRequired) {
            String token = resolveToken();
            if (token == null) {
                readyFuture.completeExceptionally(
                        new SodpException(401, "Server requires auth but no token configured"));
                return;
            }
            enqueue(Frames.encode(FrameTypes.AUTH, 0, nextSeq(), mapOf("token", token)));
        } else {
            becomeAuthenticated();
        }
    }

    private void onStateInit(Frames.DecodedFrame f) {
        JsonNode body = f.body();
        if (body == null) return;

        // Protocol: route STATE_INIT by body.state (not stream_id).
        String  key         = body.path("state").asText();
        long    version     = body.path("version").asLong();
        boolean initialized = body.path("initialized").asBoolean(true);
        JsonNode value      = body.path("value");

        // Register/update stream → key mapping from the server's assigned stream ID.
        streamToKey.put(f.streamId(), key);

        WatchEntry entry = watches.get(key);
        if (entry == null) return;

        entry.streamId = f.streamId();
        entry.update(version, initialized, value);
        fireAll(entry, value, new WatchMeta(version, initialized));
    }

    private void onDelta(Frames.DecodedFrame f) {
        String key = streamToKey.get(f.streamId());
        if (key == null) return;

        WatchEntry entry = watches.get(key);
        if (entry == null) return;

        JsonNode body    = f.body();
        if (body == null) return;
        long    version  = body.path("version").asLong();
        JsonNode opsNode = body.path("ops");

        List<DeltaOp> ops   = parseDeltaOps(opsNode);
        JsonNode      after = DeltaApplier.applyOps(entry.cachedValue, ops);

        entry.update(version, true, after);
        fireAll(entry, after, new WatchMeta(version, true));
    }

    private void onResult(Frames.DecodedFrame f) {
        JsonNode body = f.body();
        if (body == null) return;
        String callId = body.path("call_id").asText(null);
        if (callId == null) return;

        CompletableFuture<JsonNode> future = pendingCalls.remove(callId);
        if (future == null) return;

        if (body.path("success").asBoolean(false)) {
            future.complete(body.path("data"));
        } else {
            future.completeExceptionally(
                    new SodpException(body.path("data").asText("call failed")));
        }
    }

    private void onError(Frames.DecodedFrame f) {
        JsonNode body = f.body();
        if (body == null) return;
        int    code    = body.path("code").asInt();
        String message = body.path("message").asText("unknown error");

        // 401 before auth → fail the ready future (no reconnect for auth errors).
        if (code == 401 && !authenticated) {
            readyFuture.completeExceptionally(new SodpException(code, message));
            closed = true; // do not reconnect
            return;
        }

        // Fail any pending call associated with the same call_id if present.
        JsonNode callIdNode = body.path("call_id");
        if (!callIdNode.isMissingNode()) {
            CompletableFuture<JsonNode> future = pendingCalls.remove(callIdNode.asText());
            if (future != null) future.completeExceptionally(new SodpException(code, message));
        }

        LOG.fine("SODP ERROR " + code + ": " + message);
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /**
     * Returns a {@link CompletableFuture} that completes once the connection is
     * established and authenticated (or immediately if auth is disabled).
     */
    public CompletableFuture<Void> ready() {
        return readyFuture;
    }

    /**
     * Subscribe to a state key with a typed callback.
     *
     * <p>If a value is already cached, the callback fires immediately with the
     * cached value.  Returns a {@link Runnable} that cancels this subscription.
     *
     * @param key      state key (e.g. {@code "game.player"})
     * @param type     Java type for deserialization (use {@link JsonNode}{@code .class} for raw access)
     * @param callback invoked on every update
     * @return an unsubscribe handle — call it to stop receiving updates
     */
    public <T> Runnable watch(String key, Class<T> type, WatchCallback<T> callback) {
        WatchEntry entry = watches.computeIfAbsent(key, k -> new WatchEntry());
        CallbackEntry<T> cb = new CallbackEntry<>(type, callback, mapper);
        entry.addCallback(cb);

        // Fire immediately if we already have a cached value.
        JsonNode cached = entry.cachedValue;
        if (cached != null) {
            fireOne(cb, cached, new WatchMeta(entry.version, entry.initialized));
        }

        // Subscribe if connected and this key has no active stream yet.
        if (authenticated && entry.streamId == 0) {
            subscribeOrResume(key, entry);
        }
        // If not yet authenticated, becomeAuthenticated() will subscribe all entries.

        return () -> entry.removeCallback(cb);
    }

    /**
     * Subscribe to a state key; values are delivered as raw {@link JsonNode}.
     */
    public Runnable watch(String key, WatchCallback<JsonNode> callback) {
        return watch(key, JsonNode.class, callback);
    }

    /**
     * Invoke a server-side method.
     *
     * @param method one of {@code state.set}, {@code state.patch}, {@code state.set_in},
     *               {@code state.delete}, {@code state.presence}
     * @param args   method arguments
     * @return a future that resolves to the {@code data} field of the {@code RESULT} frame
     */
    public CompletableFuture<JsonNode> call(String method, Map<String, Object> args) {
        String callId = UUID.randomUUID().toString();
        CompletableFuture<JsonNode> future = new CompletableFuture<>();
        pendingCalls.put(callId, future);

        // 30-second timeout.
        scheduler.schedule(() -> {
            CompletableFuture<JsonNode> f = pendingCalls.remove(callId);
            if (f != null) f.completeExceptionally(new SodpException("Call timed out: " + method));
        }, 30, TimeUnit.SECONDS);

        Map<String, Object> body = mapOf("call_id", callId, "method", method, "args", args);
        byte[] frame = Frames.encode(FrameTypes.CALL, 0, nextSeq(), body);

        if (authenticated) {
            enqueue(frame);
        } else {
            synchronized (sendQueue) {
                sendQueue.add(frame);
            }
        }

        return future;
    }

    /** Replace the full value of a state key. */
    public CompletableFuture<JsonNode> set(String key, Object value) {
        return call("state.set", mapOf("state", key, "value", toPackable(value)));
    }

    /** Deep-merge a partial object into existing state. */
    public CompletableFuture<JsonNode> patch(String key, Object partial) {
        return call("state.patch", mapOf("state", key, "patch", toPackable(partial)));
    }

    /** Set a nested field by JSON Pointer path (e.g. {@code "/position/x"}). */
    public CompletableFuture<JsonNode> setIn(String key, String path, Object value) {
        return call("state.set_in", mapOf("state", key, "path", path, "value", toPackable(value)));
    }

    /** Remove a state key entirely. */
    public CompletableFuture<JsonNode> delete(String key) {
        return call("state.delete", mapOf("state", key));
    }

    /**
     * Set a nested path and bind it to the session lifetime.
     * The path is automatically removed when this client disconnects.
     */
    public CompletableFuture<JsonNode> presence(String key, String path, Object value) {
        return call("state.presence",
                mapOf("state", key, "path", path, "value", toPackable(value)));
    }

    /** Cancel the subscription for {@code key}. */
    public void unwatch(String key) {
        WatchEntry entry = watches.remove(key);
        if (entry == null) return;
        if (entry.streamId > 0) streamToKey.remove(entry.streamId);
        if (authenticated) {
            enqueue(Frames.encode(FrameTypes.UNWATCH, 0, nextSeq(), mapOf("state", key)));
        }
    }

    /**
     * Return the current cached snapshot for {@code key}, or {@link Optional#empty()}
     * if the key is not being watched or no value has arrived yet.
     */
    public <T> Optional<T> getSnapshot(String key, Class<T> type) {
        WatchEntry entry = watches.get(key);
        if (entry == null || entry.cachedValue == null) return Optional.empty();
        T value = type == JsonNode.class
                ? type.cast(entry.cachedValue)
                : mapper.convertValue(entry.cachedValue, type);
        return Optional.of(value);
    }

    /** Whether there is at least one active callback registered for {@code key}. */
    public boolean isWatching(String key) {
        WatchEntry entry = watches.get(key);
        return entry != null && !entry.callbacks().isEmpty();
    }

    /**
     * Get a key-scoped typed handle ({@link StateRef}) for cleaner per-key code.
     *
     * <pre>{@code
     * StateRef<PlayerState> player = client.state("game.player", PlayerState.class);
     * player.watch((value, meta) -> { ... });
     * player.set(new PlayerState(100, "Alice")).get();
     * }</pre>
     */
    public <T> StateRef<T> state(String key, Class<T> type) {
        return new StateRef<>(this, key, type);
    }

    /** Gracefully close the WebSocket and stop background threads. */
    public void close() {
        closed = true;
        senderThread.interrupt();
        scheduler.shutdownNow();
        WebSocket w = ws;
        if (w != null) {
            try {
                w.sendClose(WebSocket.NORMAL_CLOSURE, "").get(2, TimeUnit.SECONDS);
            } catch (Exception ignored) { /* best effort */ }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    private void enqueue(byte[] frame) {
        outbound.offer(frame);
    }

    private long nextSeq() {
        return seqCounter.incrementAndGet();
    }

    private String resolveToken() {
        if (tokenProvider != null) return tokenProvider.get();
        return staticToken;
    }

    /**
     * Convert a user-supplied value to something {@link Frames} can pack.
     * Primitives, Strings, Maps, Lists, and JsonNodes are passed through;
     * arbitrary POJOs are converted via {@code mapper.valueToTree()}.
     */
    private Object toPackable(Object value) {
        if (value == null)                  return null;
        if (value instanceof Boolean)       return value;
        if (value instanceof Number)        return value;
        if (value instanceof String)        return value;
        if (value instanceof JsonNode)      return value;
        if (value instanceof Map)           return value;
        if (value instanceof List)          return value;
        return mapper.valueToTree(value); // POJO → JsonNode
    }

    private void fireAll(WatchEntry entry, JsonNode value, WatchMeta meta) {
        for (CallbackEntry<?> cb : entry.callbacks()) {
            fireOne(cb, value, meta);
        }
    }

    private void fireOne(CallbackEntry<?> cb, JsonNode value, WatchMeta meta) {
        try {
            cb.fire(value, meta);
        } catch (Exception e) {
            LOG.warning("Watch callback threw: " + e.getMessage());
        }
    }

    private List<DeltaOp> parseDeltaOps(JsonNode opsNode) {
        if (opsNode == null || !opsNode.isArray()) return List.of();
        List<DeltaOp> ops = new ArrayList<>(opsNode.size());
        for (JsonNode node : opsNode) {
            String   op   = node.path("op").asText();
            String   path = node.path("path").asText();
            JsonNode val  = node.path("value"); // may be MissingNode for REMOVE
            ops.add(switch (op) {
                case "ADD"    -> new DeltaOp.Add(path, val.isMissingNode() ? null : val);
                case "UPDATE" -> new DeltaOp.Update(path, val.isMissingNode() ? null : val);
                default       -> new DeltaOp.Remove(path);
            });
        }
        return ops;
    }

    private static Map<String, Object> mapOf(Object... pairs) {
        Map<String, Object> m = new LinkedHashMap<>(pairs.length / 2);
        for (int i = 0; i < pairs.length; i += 2) {
            m.put((String) pairs[i], pairs[i + 1]);
        }
        return m;
    }

    // ── Builder ───────────────────────────────────────────────────────────────

    /**
     * Fluent builder for {@link SodpClient}.
     */
    public static final class Builder {

        private final String        url;
        private String              token;
        private Supplier<String>    tokenProvider;
        private boolean             autoReconnect = true;
        private Runnable            onConnect;
        private Runnable            onDisconnect;
        private ObjectMapper        objectMapper;

        private Builder(String url) {
            this.url = Objects.requireNonNull(url, "url must not be null");
        }

        /** Static JWT token sent in the AUTH frame. */
        public Builder token(String token) {
            this.token = token;
            return this;
        }

        /**
         * Dynamic token supplier — called on every connect/reconnect.
         * Takes precedence over {@link #token(String)}.
         */
        public Builder tokenProvider(Supplier<String> provider) {
            this.tokenProvider = provider;
            return this;
        }

        /** Whether to automatically reconnect on disconnect (default: {@code true}). */
        public Builder autoReconnect(boolean reconnect) {
            this.autoReconnect = reconnect;
            return this;
        }

        /** Callback invoked each time the connection is established and authenticated. */
        public Builder onConnect(Runnable cb) {
            this.onConnect = cb;
            return this;
        }

        /** Callback invoked each time the connection drops. */
        public Builder onDisconnect(Runnable cb) {
            this.onDisconnect = cb;
            return this;
        }

        /**
         * Supply a pre-configured {@link ObjectMapper}.  Use this in Spring Boot /
         * Quarkus / Micronaut to share the framework's configured mapper so that
         * custom serializers, modules, and naming strategies are respected.
         */
        public Builder objectMapper(ObjectMapper mapper) {
            this.objectMapper = mapper;
            return this;
        }

        /** Build and immediately start connecting. */
        public SodpClient build() {
            return new SodpClient(this);
        }
    }
}
