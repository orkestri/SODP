package io.sodp.client.delta;

/**
 * One structural change in a SODP {@code DELTA} frame.
 *
 * <p>The {@code path} is a JSON Pointer (RFC 6901), e.g. {@code /field/nested}.
 * The special path {@code "/"} means the root value itself.
 */
public sealed interface DeltaOp permits DeltaOp.Add, DeltaOp.Update, DeltaOp.Remove {

    String path();

    /** A new field that did not exist in the previous value. */
    record Add(String path, Object value) implements DeltaOp {}

    /** An existing field whose value changed. */
    record Update(String path, Object value) implements DeltaOp {}

    /** A field that was removed (no value). */
    record Remove(String path) implements DeltaOp {}
}
