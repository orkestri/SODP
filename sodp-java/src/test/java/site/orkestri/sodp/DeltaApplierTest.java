package site.orkestri.sodp;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.NullNode;
import site.orkestri.sodp.delta.DeltaApplier;
import site.orkestri.sodp.delta.DeltaOp;
import org.junit.jupiter.api.Test;

import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

class DeltaApplierTest {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private JsonNode json(String src) throws Exception {
        return MAPPER.readTree(src);
    }

    // 1 ── ADD new field ───────────────────────────────────────────────────────

    @Test
    void add_new_top_level_field() throws Exception {
        JsonNode state  = json("{\"a\":1}");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/b", 2)));

        assertEquals(1, result.path("a").asInt());
        assertEquals(2, result.path("b").asInt());
    }

    // 2 ── UPDATE existing field ───────────────────────────────────────────────

    @Test
    void update_existing_field() throws Exception {
        JsonNode state  = json("{\"health\":100}");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Update("/health", 80)));

        assertEquals(80, result.path("health").asInt());
    }

    // 3 ── REMOVE field ────────────────────────────────────────────────────────

    @Test
    void remove_field() throws Exception {
        JsonNode state  = json("{\"a\":1,\"b\":2}");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Remove("/b")));

        assertEquals(1, result.path("a").asInt());
        assertTrue(result.path("b").isMissingNode(), "field 'b' should have been removed");
    }

    // 4 ── Nested UPDATE ───────────────────────────────────────────────────────

    @Test
    void nested_update() throws Exception {
        JsonNode state  = json("{\"pos\":{\"x\":0,\"y\":0}}");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Update("/pos/x", 5)));

        assertEquals(5, result.path("pos").path("x").asInt(), "/pos/x should be 5");
        assertEquals(0, result.path("pos").path("y").asInt(), "/pos/y should be unchanged");
    }

    // 5 ── Root replacement ────────────────────────────────────────────────────

    @Test
    void root_replacement_with_slash_path() throws Exception {
        JsonNode state  = json("{\"x\":1}");
        JsonNode result = DeltaApplier.applyOps(state,
                List.of(new DeltaOp.Update("/", Map.of("y", 99))));

        assertTrue(result.path("x").isMissingNode(), "old key 'x' should be gone");
        assertEquals(99, result.path("y").asInt());
    }

    // 6 ── Root removal ────────────────────────────────────────────────────────

    @Test
    void root_remove_yields_null_node() throws Exception {
        JsonNode state  = json("{\"a\":1}");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Remove("/")));

        assertTrue(result.isNull(), "root REMOVE should return NullNode");
    }

    // 7 ── Multiple ops in order ───────────────────────────────────────────────

    @Test
    void multiple_ops_applied_in_sequence() throws Exception {
        JsonNode state = json("{\"health\":100,\"mana\":50}");

        List<DeltaOp> ops = List.of(
                new DeltaOp.Update("/health", 80),
                new DeltaOp.Add("/shield", 30),
                new DeltaOp.Remove("/mana")
        );
        JsonNode result = DeltaApplier.applyOps(state, ops);

        assertEquals(80, result.path("health").asInt());
        assertEquals(30, result.path("shield").asInt());
        assertTrue(result.path("mana").isMissingNode(), "'mana' should have been removed");
    }

    // 8 ── Immutability ────────────────────────────────────────────────────────

    @Test
    void original_state_is_not_mutated() throws Exception {
        JsonNode state  = json("{\"hp\":100}");
        JsonNode before = state.deepCopy();

        DeltaApplier.applyOps(state, List.of(new DeltaOp.Update("/hp", 50)));

        assertEquals(before, state, "original state must not be mutated");
    }

    // 9 ── null initial state ──────────────────────────────────────────────────

    @Test
    void null_initial_state_creates_object() {
        JsonNode result = DeltaApplier.applyOps(null, List.of(new DeltaOp.Add("/x", 1)));

        assertTrue(result.isObject());
        assertEquals(1, result.path("x").asInt());
    }

    // 10 ── Empty ops ──────────────────────────────────────────────────────────

    @Test
    void empty_ops_returns_state_unchanged() throws Exception {
        JsonNode state  = json("{\"a\":42}");
        JsonNode result = DeltaApplier.applyOps(state, List.of());

        assertSame(state, result, "empty ops should return the exact same reference");
    }

    // 11 ── RFC 6901 "-" array append ──────────────────────────────────────────

    @Test
    void add_append_to_root_array() throws Exception {
        JsonNode state  = json("[1,2,3]");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/-", 4)));

        assertTrue(result.isArray());
        assertEquals(4, result.size());
        assertEquals(4, result.get(3).asInt());
    }

    @Test
    void add_append_to_nested_array() throws Exception {
        JsonNode state  = json("{\"items\":[{\"id\":1}]}");
        JsonNode result = DeltaApplier.applyOps(state,
                List.of(new DeltaOp.Add("/items/-", MAPPER.readTree("{\"id\":2}"))));

        JsonNode items = result.path("items");
        assertTrue(items.isArray());
        assertEquals(2, items.size());
        assertEquals(2, items.get(1).path("id").asInt());
    }

    @Test
    void multiple_appends_apply_in_order() throws Exception {
        JsonNode state = MAPPER.createArrayNode();
        List<DeltaOp> ops = List.of(
                new DeltaOp.Add("/-", "a"),
                new DeltaOp.Add("/-", "b"),
                new DeltaOp.Add("/-", "c")
        );
        JsonNode result = DeltaApplier.applyOps(state, ops);

        assertEquals(3, result.size());
        assertEquals("a", result.get(0).asText());
        assertEquals("b", result.get(1).asText());
        assertEquals("c", result.get(2).asText());
    }

    // 12 ── Numeric array indices ──────────────────────────────────────────────

    @Test
    void update_by_numeric_index_into_array() throws Exception {
        JsonNode state  = json("[1,2,3]");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Update("/1", 99)));

        assertEquals(1, result.get(0).asInt());
        assertEquals(99, result.get(1).asInt());
        assertEquals(3, result.get(2).asInt());
    }

    @Test
    void remove_by_numeric_index_splices_array() throws Exception {
        JsonNode state  = json("[1,2,3]");
        JsonNode result = DeltaApplier.applyOps(state, List.of(new DeltaOp.Remove("/1")));

        assertEquals(2, result.size());
        assertEquals(1, result.get(0).asInt());
        assertEquals(3, result.get(1).asInt());
    }

    // 13 ── Array ops preserve immutability ────────────────────────────────────

    @Test
    void array_append_does_not_mutate_original() throws Exception {
        JsonNode state  = json("[1,2,3]");
        JsonNode before = state.deepCopy();

        DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/-", 4)));

        assertEquals(before, state, "original array must not be mutated");
    }

    // 14 ── Brokoli regression: null state + array paths ──────────────────────
    // Before this fix, ADD "/-" on null state produced {"-": value} because
    // the root fallback was always ObjectNode regardless of the path shape.

    @Test
    void add_append_on_null_state_initializes_root_array() {
        JsonNode result = DeltaApplier.applyOps(null, List.of(new DeltaOp.Add("/-", "x")));
        assertTrue(result.isArray(), "root must be an array, not an object");
        assertEquals(1, result.size());
        assertEquals("x", result.get(0).asText());
    }

    @Test
    void add_numeric_index_on_null_state_initializes_root_array() {
        JsonNode result = DeltaApplier.applyOps(null, List.of(new DeltaOp.Add("/0", "x")));
        assertTrue(result.isArray());
        assertEquals("x", result.get(0).asText());
    }

    @Test
    void add_nested_append_on_null_state_materializes_nested_array() {
        JsonNode result = DeltaApplier.applyOps(null,
                List.of(new DeltaOp.Add("/items/-", "x")));
        assertTrue(result.isObject());
        assertTrue(result.path("items").isArray());
        assertEquals("x", result.path("items").get(0).asText());
    }

    @Test
    void consecutive_appends_on_null_grow_an_array() {
        JsonNode state = null;
        state = DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/-", "a")));
        state = DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/-", "b")));
        state = DeltaApplier.applyOps(state, List.of(new DeltaOp.Add("/-", "c")));
        assertTrue(state.isArray());
        assertEquals(3, state.size());
        assertEquals("a", state.get(0).asText());
        assertEquals("b", state.get(1).asText());
        assertEquals("c", state.get(2).asText());
    }

    @Test
    void object_path_on_null_state_still_materializes_an_object() {
        // Sanity: the array-preference must not leak into object paths.
        JsonNode result = DeltaApplier.applyOps(null,
                List.of(new DeltaOp.Add("/name", "Alice")));
        assertTrue(result.isObject());
        assertEquals("Alice", result.path("name").asText());
    }
}
