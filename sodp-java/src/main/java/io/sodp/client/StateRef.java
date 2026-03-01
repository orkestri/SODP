package io.sodp.client;

import com.fasterxml.jackson.databind.JsonNode;

import java.util.Optional;
import java.util.concurrent.CompletableFuture;

/**
 * A key-scoped typed handle that bundles {@link SodpClient} operations for a
 * single state key.
 *
 * <p>Obtain a {@code StateRef} from {@link SodpClient#state(String, Class)}:
 *
 * <pre>{@code
 * StateRef<PlayerState> player = client.state("game.player", PlayerState.class);
 *
 * player.watch((value, meta) ->
 *     System.out.println("health=" + value.health() + " v" + meta.version()));
 *
 * player.set(new PlayerState(80, "Alice")).get();
 * player.setIn("/position/x", 5).get();
 * player.delete().get();
 * }</pre>
 *
 * @param <T> the Java type of the state value
 */
public final class StateRef<T> {

    private final SodpClient client;
    private final String     key;
    private final Class<T>   type;

    StateRef(SodpClient client, String key, Class<T> type) {
        this.client = client;
        this.key    = key;
        this.type   = type;
    }

    /** The state key this handle is bound to. */
    public String key() { return key; }

    /**
     * Register a callback for updates to this key.
     *
     * @return an unsubscribe handle — call it to stop receiving updates
     * @see SodpClient#watch(String, Class, WatchCallback)
     */
    public Runnable watch(WatchCallback<T> callback) {
        return client.watch(key, type, callback);
    }

    /**
     * Return the current cached snapshot, or {@link Optional#empty()} if no
     * value has been received yet.
     */
    public Optional<T> get() {
        return client.getSnapshot(key, type);
    }

    /** Whether at least one callback is actively registered for this key. */
    public boolean isWatching() {
        return client.isWatching(key);
    }

    /** Cancel the subscription for this key. */
    public void unwatch() {
        client.unwatch(key);
    }

    // ── Mutation methods ──────────────────────────────────────────────────────

    /** Replace the full value of this key. */
    public CompletableFuture<JsonNode> set(T value) {
        return client.set(key, value);
    }

    /**
     * Deep-merge a partial object into the existing value.
     * {@code partial} may be a {@code Map<String, Object>} or a compatible POJO.
     */
    public CompletableFuture<JsonNode> patch(Object partial) {
        return client.patch(key, partial);
    }

    /** Set a nested field by JSON Pointer path (e.g. {@code "/position/x"}). */
    public CompletableFuture<JsonNode> setIn(String path, Object value) {
        return client.setIn(key, path, value);
    }

    /** Remove this key from the server state entirely. */
    public CompletableFuture<JsonNode> delete() {
        return client.delete(key);
    }

    /**
     * Set a nested path and bind it to the client's session lifetime.
     * The value is automatically removed when this client disconnects.
     *
     * @param path  JSON Pointer path (e.g. {@code "/cursors/user123"})
     * @param value value to write at that path
     */
    public CompletableFuture<JsonNode> presence(String path, Object value) {
        return client.presence(key, path, value);
    }
}
