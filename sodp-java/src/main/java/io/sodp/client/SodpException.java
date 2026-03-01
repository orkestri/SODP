package io.sodp.client;

/**
 * Thrown (or used as a {@link java.util.concurrent.CompletableFuture} failure cause)
 * when the SODP server returns an error frame or a call times out.
 */
public class SodpException extends RuntimeException {

    private final int code;

    /** Server error with an HTTP-style status code. */
    public SodpException(int code, String message) {
        super("SODP error " + code + ": " + message);
        this.code = code;
    }

    /** Client-side error (e.g. call timeout, encode failure). */
    public SodpException(String message) {
        super(message);
        this.code = -1;
    }

    /**
     * HTTP-style error code from the server (400, 401, 403, 422, 429 …),
     * or {@code -1} for client-side errors.
     */
    public int code() {
        return code;
    }
}
