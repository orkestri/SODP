package io.sodp.client;

/**
 * Origin of a watch callback invocation.
 *
 * <p>Use this — not {@link WatchMeta#initialized()} — to distinguish the
 * initial baseline from subsequent changes. {@code initialized} only tells
 * you whether the key has ever been written on the server; {@code source}
 * tells you which event produced this particular callback.
 */
public enum WatchSource {
    /** Fired synchronously from {@code watch()} with an already-cached value. */
    CACHE,
    /** The server's {@code STATE_INIT} baseline (initial load or post-reconnect). */
    INIT,
    /** An incremental mutation from a {@code DELTA} frame. */
    DELTA,
}
