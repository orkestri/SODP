package io.sodp.client;

/**
 * Called whenever the value of a watched state key changes.
 *
 * <p>Invoked on the client's internal receive thread.  Implementations should
 * be non-blocking; hand off to an executor if heavy work is needed.
 *
 * @param <T> the type of the state value
 */
@FunctionalInterface
public interface WatchCallback<T> {
    void onUpdate(T value, WatchMeta meta);
}
