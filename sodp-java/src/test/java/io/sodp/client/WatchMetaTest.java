package io.sodp.client;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class WatchMetaTest {

    @Test
    void recordFields() {
        WatchMeta meta = new WatchMeta(42L, true, WatchSource.DELTA);
        assertEquals(42L, meta.version());
        assertTrue(meta.initialized());
        assertEquals(WatchSource.DELTA, meta.source());
    }

    @Test
    void equalityAndHashCode() {
        WatchMeta a = new WatchMeta(1, true, WatchSource.INIT);
        WatchMeta b = new WatchMeta(1, true, WatchSource.INIT);
        WatchMeta c = new WatchMeta(2, false, WatchSource.INIT);
        WatchMeta d = new WatchMeta(1, true, WatchSource.DELTA);

        assertEquals(a, b);
        assertEquals(a.hashCode(), b.hashCode());
        assertNotEquals(a, c);
        assertNotEquals(a, d, "source is part of the record's identity");
    }

    @Test
    void uninitializedMeta() {
        WatchMeta meta = new WatchMeta(0, false, WatchSource.INIT);
        assertEquals(0L, meta.version());
        assertFalse(meta.initialized());
        assertEquals(WatchSource.INIT, meta.source());
    }

    @Test
    void allSourcesDistinguishable() {
        // Round-trip every enum value to make sure none collide with each other
        // and that the record exposes them verbatim.
        for (WatchSource src : WatchSource.values()) {
            WatchMeta m = new WatchMeta(1, true, src);
            assertEquals(src, m.source());
        }
    }
}
