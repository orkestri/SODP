package io.sodp.client;

/**
 * Metadata delivered alongside every state update.
 *
 * @param version     monotonically increasing version of this key on the server
 * @param initialized {@code false} when the key has never been written (value is null);
 *                    {@code true} on every subsequent update
 */
public record WatchMeta(long version, boolean initialized) {}
