package io.sodp.client.delta;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.NullNode;
import com.fasterxml.jackson.databind.node.ObjectNode;

import java.util.List;

/**
 * Applies a list of {@link DeltaOp}s to a Jackson {@link JsonNode} state snapshot.
 *
 * <p>Operations are applied in order.  The input node is never mutated — a
 * {@link JsonNode#deepCopy() deep copy} is made before any structural write,
 * so callers may safely hold references to the previous value.
 */
public final class DeltaApplier {

    private DeltaApplier() {}

    private static final ObjectMapper MAPPER = new ObjectMapper();

    /**
     * Apply all ops in order and return the resulting document.
     *
     * @param state initial state (may be {@code null} — treated as empty object)
     * @param ops   delta operations from a SODP DELTA frame
     * @return the new state after all ops have been applied
     */
    public static JsonNode applyOps(JsonNode state, List<DeltaOp> ops) {
        if (ops.isEmpty()) return state;
        for (DeltaOp op : ops) {
            state = applyOne(state, op);
        }
        return state;
    }

    private static JsonNode applyOne(JsonNode state, DeltaOp op) {
        String path = op.path();
        boolean isRoot = "/".equals(path) || "".equals(path);

        if (isRoot) {
            if (op instanceof DeltaOp.Add add)       return toNode(add.value());
            if (op instanceof DeltaOp.Update upd)    return toNode(upd.value());
            return NullNode.getInstance(); // Remove
        }

        // "/a/b/c" → ["a", "b", "c"]
        String[] parts = path.substring(1).split("/", -1);
        // Deep-copy before any structural mutation so the original is preserved.
        JsonNode copy = (state != null) ? state.deepCopy() : MAPPER.createObjectNode();
        return setAt(copy, parts, 0, op);
    }

    private static JsonNode setAt(JsonNode node, String[] parts, int idx, DeltaOp op) {
        String key    = parts[idx];
        boolean isLast = idx == parts.length - 1;

        // Coerce to ObjectNode so we can write fields.
        ObjectNode obj = (node != null && node.isObject())
                ? (ObjectNode) node
                : MAPPER.createObjectNode();

        if (isLast) {
            if (op instanceof DeltaOp.Add add)         obj.set(key, toNode(add.value()));
            else if (op instanceof DeltaOp.Update upd) obj.set(key, toNode(upd.value()));
            else                                        obj.remove(key); // Remove
        } else {
            JsonNode child = obj.get(key);
            obj.set(key, setAt(child, parts, idx + 1, op));
        }

        return obj;
    }

    /** Convert an arbitrary value to a {@link JsonNode}. */
    static JsonNode toNode(Object value) {
        if (value instanceof JsonNode jn) return jn;
        return MAPPER.valueToTree(value);
    }
}
