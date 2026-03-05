package io.sodp.client;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.*;

class SodpExceptionTest {

    @Test
    void serverErrorContainsCodeAndMessage() {
        SodpException ex = new SodpException(401, "unauthorized");
        assertEquals(401, ex.code());
        assertTrue(ex.getMessage().contains("401"));
        assertTrue(ex.getMessage().contains("unauthorized"));
    }

    @Test
    void clientErrorHasNegativeCode() {
        SodpException ex = new SodpException("timeout");
        assertEquals(-1, ex.code());
        assertEquals("timeout", ex.getMessage());
    }

    @Test
    void isRuntimeException() {
        SodpException ex = new SodpException(500, "internal");
        assertInstanceOf(RuntimeException.class, ex);
    }
}
