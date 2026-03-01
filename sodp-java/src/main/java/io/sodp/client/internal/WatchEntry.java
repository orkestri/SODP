package io.sodp.client.internal;

import com.fasterxml.jackson.databind.JsonNode;

import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Per-key mutable state held by the client.
 *
 * <p>All fields are {@code volatile} so that the receive thread (which writes)
 * and user threads (which read via {@code getSnapshot}) see consistent values
 * without explicit locking.
 */
public class WatchEntry {

    /**
     * Active subscription stream ID.
     * <ul>
     *   <li>0 = not yet subscribed (initial state, or reset after disconnect)</li>
     *   <li>&gt;0 = active stream</li>
     * </ul>
     */
    public volatile int streamId = 0;

    /** Last received version (used as {@code since_version} in RESUME). */
    public volatile long version = 0;

    /** {@code false} until the server has confirmed the key was ever written. */
    public volatile boolean initialized = false;

    /** Most recent full value from STATE_INIT or accumulated from DELTAs. */
    public volatile JsonNode cachedValue = null;

    /**
     * Registered callbacks.  {@link CopyOnWriteArrayList} lets the receive
     * thread iterate without locking while user threads add/remove callbacks.
     */
    private final CopyOnWriteArrayList<CallbackEntry<?>> callbacks = new CopyOnWriteArrayList<>();

    public void update(long version, boolean initialized, JsonNode value) {
        this.version     = version;
        this.initialized = initialized;
        this.cachedValue = value;
    }

    public void addCallback(CallbackEntry<?> cb)    { callbacks.add(cb); }
    public void removeCallback(CallbackEntry<?> cb) { callbacks.remove(cb); }
    public List<CallbackEntry<?>> callbacks()        { return callbacks; }
}
