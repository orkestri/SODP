package io.sodp.client.internal;

/** SODP wire frame type codes. */
public final class FrameTypes {
    public static final int HELLO      = 0x01;
    public static final int WATCH      = 0x02;
    public static final int STATE_INIT = 0x03;
    public static final int DELTA      = 0x04;
    public static final int CALL       = 0x05;
    public static final int RESULT     = 0x06;
    public static final int ERROR      = 0x07;
    public static final int ACK        = 0x08;
    public static final int HEARTBEAT  = 0x09;
    public static final int RESUME     = 0x0A;
    public static final int AUTH       = 0x0B;
    public static final int AUTH_OK    = 0x0C;
    public static final int UNWATCH    = 0x0D;

    private FrameTypes() {}
}
