package site.orkestri.sodp.delta;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.NullNode;
import com.fasterxml.jackson.databind.node.ObjectNode;

import java.util.List;

/**
 * Applies a list of {@link DeltaOp}s to a Jackson {@link JsonNode} state snapshot.
 *
 * <p>Operations are applied in order. The input node is never mutated — a
 * {@link JsonNode#deepCopy() deep copy} is made before any structural write,
 * so callers may safely hold references to the previous value.
 *
 * <p>Paths follow JSON Pointer (RFC 6901):
 * <ul>
 *   <li>{@code "/"} — the root value itself</li>
 *   <li>{@code "/x/y"} — nested field</li>
 *   <li>{@code "/-"} — element past the end of an array (append)</li>
 *   <li>{@code "/3"} — index 3 into an array</li>
 * </ul>
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

        // Deep-copy the top-level container before any structural mutation.
        // If state is null or not a container, seed a fresh container whose
        // shape is implied by the first path segment — "-" or a numeric index
        // means the root should be an array, anything else means an object.
        // This matters when subscribing to a key before its first write.
        JsonNode copy;
        if (state != null && (state.isObject() || state.isArray())) {
            copy = state.deepCopy();
        } else {
            copy = containerFor(parts[0]);
        }
        return setAt(copy, parts, 0, op);
    }

    /** Walk down to parts[idx]; once at the leaf, delegate to {@link #writeLeaf}. */
    private static JsonNode setAt(JsonNode node, String[] parts, int idx, DeltaOp op) {
        String key    = parts[idx];
        boolean isLast = idx == parts.length - 1;

        if (isLast) {
            return writeLeaf(node, key, op);
        }

        String nextSeg = parts[idx + 1];

        // Descend into an array child.
        if (node instanceof ArrayNode arr) {
            Integer i = parseIndex(key);
            if (i == null || i < 0 || i >= arr.size()) {
                // Non-indexable or out-of-range segment on an array — no-op.
                return arr;
            }
            JsonNode child = arr.get(i);
            if (child == null || !(child.isObject() || child.isArray())) {
                child = containerFor(nextSeg);
                arr.set(i, child);
            }
            arr.set(i, setAt(child, parts, idx + 1, op));
            return arr;
        }

        // Descend into an object child (coerce to object if needed).
        ObjectNode obj = (node != null && node.isObject())
                ? (ObjectNode) node
                : MAPPER.createObjectNode();
        JsonNode child = obj.get(key);
        if (child == null || !(child.isObject() || child.isArray())) {
            child = containerFor(nextSeg);
            obj.set(key, child);
        }
        obj.set(key, setAt(child, parts, idx + 1, op));
        return obj;
    }

    /**
     * Build an empty container whose type is implied by {@code segment}:
     * {@code "-"} or a purely-numeric segment → {@link ArrayNode}, anything
     * else → {@link ObjectNode}.
     */
    private static JsonNode containerFor(String segment) {
        if ("-".equals(segment)) return MAPPER.createArrayNode();
        if (segment != null && !segment.isEmpty() && parseIndex(segment) != null) {
            return MAPPER.createArrayNode();
        }
        return MAPPER.createObjectNode();
    }

    /** Apply {@code op} to {@code parent} at {@code key} (the final path segment). */
    private static JsonNode writeLeaf(JsonNode parent, String key, DeltaOp op) {
        // RFC 6901 "-" append.
        if ("-".equals(key) && parent instanceof ArrayNode arr) {
            if (op instanceof DeltaOp.Add add)         arr.add(toNode(add.value()));
            else if (op instanceof DeltaOp.Update upd) arr.add(toNode(upd.value()));
            else if (arr.size() > 0)                   arr.remove(arr.size() - 1);
            return arr;
        }

        // Numeric index into an array.
        if (parent instanceof ArrayNode arr) {
            Integer i = parseIndex(key);
            if (i == null) return arr;
            if (op instanceof DeltaOp.Add add) {
                if (i >= 0 && i < arr.size())      arr.set(i, toNode(add.value()));
                else if (i == arr.size())          arr.add(toNode(add.value()));
            } else if (op instanceof DeltaOp.Update upd) {
                if (i >= 0 && i < arr.size())      arr.set(i, toNode(upd.value()));
                else if (i == arr.size())          arr.add(toNode(upd.value()));
            } else { // Remove
                if (i >= 0 && i < arr.size())      arr.remove(i);
            }
            return arr;
        }

        // Object leaf (coerce non-objects to an empty ObjectNode for ADD/UPDATE).
        ObjectNode obj = (parent != null && parent.isObject())
                ? (ObjectNode) parent
                : MAPPER.createObjectNode();
        if (op instanceof DeltaOp.Add add)         obj.set(key, toNode(add.value()));
        else if (op instanceof DeltaOp.Update upd) obj.set(key, toNode(upd.value()));
        else                                        obj.remove(key); // Remove
        return obj;
    }

    /** Parse a path segment as a non-negative array index, or {@code null}. */
    private static Integer parseIndex(String s) {
        if (s == null || s.isEmpty()) return null;
        for (int i = 0; i < s.length(); i++) {
            if (!Character.isDigit(s.charAt(i))) return null;
        }
        try {
            return Integer.parseInt(s);
        } catch (NumberFormatException e) {
            return null;
        }
    }

    /** Convert an arbitrary value to a {@link JsonNode}. */
    static JsonNode toNode(Object value) {
        if (value instanceof JsonNode jn) return jn;
        return MAPPER.valueToTree(value);
    }
}
