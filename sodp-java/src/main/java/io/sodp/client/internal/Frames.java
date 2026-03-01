package io.sodp.client.internal;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.*;
import org.msgpack.core.*;
import org.msgpack.value.ValueType;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.util.Map;

/**
 * Encode and decode SODP wire frames.
 *
 * <p>A frame is a 4-element MessagePack array:
 * {@code [frame_type: u8, stream_id: u32, seq: u64, body: map | null]}
 */
public final class Frames {

    private Frames() {}

    private static final ObjectMapper MAPPER = new ObjectMapper();

    // ── Decoded frame ─────────────────────────────────────────────────────────

    /** A fully decoded SODP frame. */
    public record DecodedFrame(int frameType, int streamId, long seq, JsonNode body) {}

    // ── Encoding ──────────────────────────────────────────────────────────────

    /**
     * Encode a frame whose body is a {@code Map<String, Object>}.
     * Values may be primitives, {@link JsonNode}, {@link java.util.List},
     * other {@code Map}s, or POJOs (serialized via Jackson as fallback).
     */
    public static byte[] encode(int frameType, int streamId, long seq, Map<String, Object> body) {
        try {
            ByteArrayOutputStream baos = new ByteArrayOutputStream(128);
            MessagePacker p = MessagePack.newDefaultPacker(baos);
            p.packArrayHeader(4);
            p.packInt(frameType);
            p.packInt(streamId);
            p.packLong(seq);
            packMap(p, body);
            p.close();
            return baos.toByteArray();
        } catch (IOException e) {
            throw new IllegalStateException("Frame encoding failed", e);
        }
    }

    /** Encode a frame with a {@code null} body (e.g. HEARTBEAT). */
    public static byte[] encodeNull(int frameType, int streamId, long seq) {
        try {
            ByteArrayOutputStream baos = new ByteArrayOutputStream(16);
            MessagePacker p = MessagePack.newDefaultPacker(baos);
            p.packArrayHeader(4);
            p.packInt(frameType);
            p.packInt(streamId);
            p.packLong(seq);
            p.packNil();
            p.close();
            return baos.toByteArray();
        } catch (IOException e) {
            throw new IllegalStateException("Frame encoding failed", e);
        }
    }

    private static void packMap(MessagePacker p, Map<String, Object> map) throws IOException {
        p.packMapHeader(map.size());
        for (Map.Entry<String, Object> e : map.entrySet()) {
            p.packString(e.getKey());
            packValue(p, e.getValue());
        }
    }

    @SuppressWarnings("unchecked")
    private static void packValue(MessagePacker p, Object value) throws IOException {
        if (value == null) {
            p.packNil();
        } else if (value instanceof Boolean b) {
            p.packBoolean(b);
        } else if (value instanceof Integer i) {
            p.packInt(i);
        } else if (value instanceof Long l) {
            p.packLong(l);
        } else if (value instanceof Float f) {
            p.packFloat(f);
        } else if (value instanceof Double d) {
            p.packDouble(d);
        } else if (value instanceof Number n) {
            p.packDouble(n.doubleValue());
        } else if (value instanceof String s) {
            p.packString(s);
        } else if (value instanceof JsonNode jn) {
            packJsonNode(p, jn);
        } else if (value instanceof java.util.List<?> list) {
            p.packArrayHeader(list.size());
            for (Object item : list) packValue(p, item);
        } else if (value instanceof Map<?, ?> map) {
            p.packMapHeader(map.size());
            for (Map.Entry<?, ?> e : map.entrySet()) {
                p.packString(e.getKey().toString());
                packValue(p, e.getValue());
            }
        } else {
            // POJO fallback: JSON-serialize → pack as string.
            // In normal SODP usage callers pass primitives, Maps, Lists, or JsonNode.
            try {
                packJsonNode(p, MAPPER.valueToTree(value));
            } catch (Exception ex) {
                p.packString(value.toString());
            }
        }
    }

    private static void packJsonNode(MessagePacker p, JsonNode node) throws IOException {
        if (node == null || node.isNull())    { p.packNil(); }
        else if (node.isBoolean())            { p.packBoolean(node.booleanValue()); }
        else if (node.isInt())                { p.packInt(node.intValue()); }
        else if (node.isLong())               { p.packLong(node.longValue()); }
        else if (node.isFloat())              { p.packFloat(node.floatValue()); }
        else if (node.isDouble())             { p.packDouble(node.doubleValue()); }
        else if (node.isBigInteger())         { p.packLong(node.longValue()); }
        else if (node.isBigDecimal())         { p.packDouble(node.doubleValue()); }
        else if (node.isTextual())            { p.packString(node.textValue()); }
        else if (node.isArray()) {
            p.packArrayHeader(node.size());
            for (JsonNode child : node) packJsonNode(p, child);
        } else if (node.isObject()) {
            p.packMapHeader(node.size());
            var it = node.fields();
            while (it.hasNext()) {
                var e = it.next();
                p.packString(e.getKey());
                packJsonNode(p, e.getValue());
            }
        } else {
            p.packNil(); // binary etc. — not used by SODP
        }
    }

    // ── Decoding ──────────────────────────────────────────────────────────────

    /**
     * Decode a raw MessagePack binary frame.
     *
     * @throws IllegalArgumentException if the bytes are not a valid 4-element frame
     */
    public static DecodedFrame decode(byte[] bytes) {
        try {
            MessageUnpacker u = MessagePack.newDefaultUnpacker(bytes);
            int arrayLen = u.unpackArrayHeader();
            if (arrayLen != 4) {
                throw new IllegalArgumentException("Expected 4-element array, got " + arrayLen);
            }
            int      frameType = u.unpackInt();
            int      streamId  = (int) u.unpackLong(); // u32 — unpack as long for safety
            long     seq       = u.unpackLong();
            JsonNode body      = unpackNode(u);
            u.close();
            return new DecodedFrame(frameType, streamId, seq, body);
        } catch (IOException e) {
            throw new IllegalArgumentException("Frame decoding failed: " + e.getMessage(), e);
        }
    }

    private static JsonNode unpackNode(MessageUnpacker u) throws IOException {
        ValueType type = u.getNextFormat().getValueType();
        return switch (type) {
            case NIL -> {
                u.unpackNil();
                yield NullNode.getInstance();
            }
            case BOOLEAN -> BooleanNode.valueOf(u.unpackBoolean());
            case INTEGER -> {
                long v = u.unpackLong();
                yield (v >= Integer.MIN_VALUE && v <= Integer.MAX_VALUE)
                      ? IntNode.valueOf((int) v)
                      : LongNode.valueOf(v);
            }
            case FLOAT -> DoubleNode.valueOf(u.unpackDouble());
            case STRING -> TextNode.valueOf(u.unpackString());
            case BINARY -> {
                int len = u.unpackBinaryHeader();
                yield BinaryNode.valueOf(u.readPayload(len));
            }
            case ARRAY -> {
                int len = u.unpackArrayHeader();
                ArrayNode arr = MAPPER.createArrayNode();
                for (int i = 0; i < len; i++) arr.add(unpackNode(u));
                yield arr;
            }
            case MAP -> {
                int len = u.unpackMapHeader();
                ObjectNode obj = MAPPER.createObjectNode();
                for (int i = 0; i < len; i++) {
                    String key = u.unpackString();
                    obj.set(key, unpackNode(u));
                }
                yield obj;
            }
            case EXTENSION -> {
                // SODP does not use MessagePack extensions — skip and return null.
                ExtensionTypeHeader hdr = u.unpackExtensionTypeHeader();
                u.readPayload(hdr.getLength());
                yield NullNode.getInstance();
            }
        };
    }
}
