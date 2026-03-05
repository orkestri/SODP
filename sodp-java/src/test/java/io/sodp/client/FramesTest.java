package io.sodp.client;

import com.fasterxml.jackson.databind.JsonNode;
import io.sodp.client.internal.Frames;
import io.sodp.client.internal.FrameTypes;
import org.junit.jupiter.api.Test;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Tests for MessagePack frame encoding/decoding.
 */
class FramesTest {

    // ── Round-trip ──────────────────────────────────────────────────────────────

    @Test
    void roundTripSimpleBody() {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("state", "game.score");
        body.put("value", 42);

        byte[] encoded = Frames.encode(FrameTypes.CALL, 0, 1L, body);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        assertEquals(FrameTypes.CALL, frame.frameType());
        assertEquals(0, frame.streamId());
        assertEquals(1L, frame.seq());
        assertEquals("game.score", frame.body().path("state").asText());
        assertEquals(42, frame.body().path("value").asInt());
    }

    @Test
    void roundTripNestedMap() {
        Map<String, Object> inner = Map.of("x", 1.5, "y", 2.5);
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("state", "game.player");
        body.put("value", Map.of("name", "Alice", "position", inner));

        byte[] encoded = Frames.encode(FrameTypes.CALL, 5, 10L, body);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        assertEquals(5, frame.streamId());
        JsonNode value = frame.body().path("value");
        assertEquals("Alice", value.path("name").asText());
        assertEquals(1.5, value.path("position").path("x").asDouble(), 0.001);
    }

    @Test
    void roundTripWithList() {
        Map<String, Object> body = Map.of("items", List.of("a", "b", "c"));

        byte[] encoded = Frames.encode(FrameTypes.CALL, 0, 1L, body);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        JsonNode items = frame.body().path("items");
        assertTrue(items.isArray());
        assertEquals(3, items.size());
        assertEquals("b", items.get(1).asText());
    }

    @Test
    void roundTripNullBody() {
        byte[] encoded = Frames.encodeNull(FrameTypes.HEARTBEAT, 0, 0L);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        assertEquals(FrameTypes.HEARTBEAT, frame.frameType());
        assertTrue(frame.body().isNull());
    }

    @Test
    void roundTripBooleans() {
        Map<String, Object> body = Map.of("flag", true, "other", false);

        byte[] encoded = Frames.encode(FrameTypes.CALL, 0, 1L, body);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        assertTrue(frame.body().path("flag").asBoolean());
        assertFalse(frame.body().path("other").asBoolean());
    }

    @Test
    void roundTripNullValue() {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("key", "test");
        body.put("value", null);

        byte[] encoded = Frames.encode(FrameTypes.CALL, 0, 1L, body);
        Frames.DecodedFrame frame = Frames.decode(encoded);

        assertTrue(frame.body().path("value").isNull());
    }

    // ── Error cases ─────────────────────────────────────────────────────────────

    @Test
    void decodeEmptyBytesThrows() {
        assertThrows(Exception.class, () -> Frames.decode(new byte[]{}));
    }

    // ── Frame types ─────────────────────────────────────────────────────────────

    @Test
    void allFrameTypeConstantsAreDistinct() {
        int[] types = {
            FrameTypes.HELLO, FrameTypes.WATCH, FrameTypes.STATE_INIT,
            FrameTypes.DELTA, FrameTypes.CALL, FrameTypes.RESULT,
            FrameTypes.ERROR, FrameTypes.HEARTBEAT, FrameTypes.RESUME,
            FrameTypes.AUTH, FrameTypes.AUTH_OK, FrameTypes.UNWATCH
        };

        // Check uniqueness
        for (int i = 0; i < types.length; i++) {
            for (int j = i + 1; j < types.length; j++) {
                assertNotEquals(types[i], types[j],
                    "Frame type constants at index " + i + " and " + j + " collide");
            }
        }
    }
}
