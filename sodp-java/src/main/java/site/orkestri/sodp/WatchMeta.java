package site.orkestri.sodp;

/**
 * Metadata delivered alongside every state update.
 *
 * @param version     monotonically increasing version of this key on the server
 * @param initialized {@code false} when the key has never been written (value is null);
 *                    {@code true} on every subsequent update
 * @param source      origin of this callback invocation — see {@link WatchSource}.
 *                    Use this, not {@code initialized}, to distinguish the initial
 *                    baseline from subsequent mutations.
 */
public record WatchMeta(long version, boolean initialized, WatchSource source) {}
