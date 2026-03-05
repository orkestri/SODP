package io.sodp.client;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class WatchMetaTest {

    @Test
    void recordFields() {
        WatchMeta meta = new WatchMeta(42L, true);
        assertEquals(42L, meta.version());
        assertTrue(meta.initialized());
    }

    @Test
    void equalityAndHashCode() {
        WatchMeta a = new WatchMeta(1, true);
        WatchMeta b = new WatchMeta(1, true);
        WatchMeta c = new WatchMeta(2, false);

        assertEquals(a, b);
        assertEquals(a.hashCode(), b.hashCode());
        assertNotEquals(a, c);
    }

    @Test
    void uninitializedMeta() {
        WatchMeta meta = new WatchMeta(0, false);
        assertEquals(0L, meta.version());
        assertFalse(meta.initialized());
    }
}
