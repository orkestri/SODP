package io.sodp.client.internal;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.sodp.client.WatchCallback;

/**
 * Pairs a typed {@link WatchCallback} with the {@link Class} used to
 * deserialize incoming {@link JsonNode} values into {@code T}.
 */
public record CallbackEntry<T>(Class<T> type, WatchCallback<T> callback, ObjectMapper mapper) {

    /**
     * Convert a {@link JsonNode} to {@code T} and invoke the callback.
     * If {@code T} is {@code JsonNode} itself, the node is passed through
     * without conversion.
     */
    @SuppressWarnings("unchecked")
    public void fire(JsonNode node, io.sodp.client.WatchMeta meta) {
        T value = type == JsonNode.class
                ? (T) node
                : mapper.convertValue(node, type);
        callback.onUpdate(value, meta);
    }
}
