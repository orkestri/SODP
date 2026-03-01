package io.sodp.client;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.NullNode;
import io.sodp.client.delta.DeltaApplier;
import io.sodp.client.delta.DeltaOp;
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
}
