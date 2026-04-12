import { describe, it, expect } from "@jest/globals";
import { applyOps, type DeltaOp } from "./delta.js";

describe("applyOps", () => {
  it("ADD at root replaces entire state", () => {
    const result = applyOps(null, [{ op: "ADD", path: "/", value: { x: 1 } }]);
    expect(result).toEqual({ x: 1 });
  });

  it("UPDATE at root replaces entire state", () => {
    const result = applyOps({ x: 1 }, [{ op: "UPDATE", path: "/", value: { x: 2 } }]);
    expect(result).toEqual({ x: 2 });
  });

  it("REMOVE at root returns null", () => {
    const result = applyOps({ x: 1 }, [{ op: "REMOVE", path: "/" }]);
    expect(result).toBeNull();
  });

  it("ADD top-level field", () => {
    const result = applyOps({ a: 1 }, [{ op: "ADD", path: "/b", value: 2 }]);
    expect(result).toEqual({ a: 1, b: 2 });
  });

  it("UPDATE top-level field", () => {
    const result = applyOps({ a: 1, b: 2 }, [{ op: "UPDATE", path: "/a", value: 99 }]);
    expect(result).toEqual({ a: 99, b: 2 });
  });

  it("REMOVE top-level field", () => {
    const result = applyOps({ a: 1, b: 2 }, [{ op: "REMOVE", path: "/b" }]);
    expect(result).toEqual({ a: 1 });
  });

  it("ADD nested field, creating intermediate objects", () => {
    const result = applyOps({}, [{ op: "ADD", path: "/x/y/z", value: 42 }]);
    expect(result).toEqual({ x: { y: { z: 42 } } });
  });

  it("UPDATE nested field", () => {
    const state = { player: { pos: { x: 0, y: 0 } } };
    const result = applyOps(state, [{ op: "UPDATE", path: "/player/pos/x", value: 5 }]);
    expect(result).toEqual({ player: { pos: { x: 5, y: 0 } } });
  });

  it("REMOVE nested field", () => {
    const state = { a: { b: 1, c: 2 } };
    const result = applyOps(state, [{ op: "REMOVE", path: "/a/b" }]);
    expect(result).toEqual({ a: { c: 2 } });
  });

  it("multiple ops applied in order", () => {
    const ops: DeltaOp[] = [
      { op: "ADD",    path: "/a", value: 1 },
      { op: "ADD",    path: "/b", value: 2 },
      { op: "UPDATE", path: "/a", value: 10 },
      { op: "REMOVE", path: "/b" },
    ];
    const result = applyOps({}, ops);
    expect(result).toEqual({ a: 10 });
  });

  it("does not mutate the original state", () => {
    const original = { x: 1 };
    applyOps(original, [{ op: "UPDATE", path: "/x", value: 99 }]);
    expect(original).toEqual({ x: 1 });
  });

  it("handles null initial state for nested ADD", () => {
    const result = applyOps(null, [{ op: "ADD", path: "/a", value: 1 }]);
    expect(result).toEqual({ a: 1 });
  });

  it("empty ops list returns state unchanged", () => {
    const state = { x: 1 };
    const result = applyOps(state, []);
    expect(result).toBe(state); // same reference when no ops
  });

  // ── RFC 6901 array append token "-" ─────────────────────────────────────────

  it("ADD /- appends to a root array", () => {
    const result = applyOps([1, 2, 3], [{ op: "ADD", path: "/-", value: 4 }]);
    expect(result).toEqual([1, 2, 3, 4]);
  });

  it("ADD nested/- appends to a nested array", () => {
    const state = { items: [{ id: 1 }] };
    const result = applyOps(state, [{ op: "ADD", path: "/items/-", value: { id: 2 } }]);
    expect(result).toEqual({ items: [{ id: 1 }, { id: 2 }] });
  });

  it("multiple /- appends apply in order", () => {
    const result = applyOps([], [
      { op: "ADD", path: "/-", value: "a" },
      { op: "ADD", path: "/-", value: "b" },
      { op: "ADD", path: "/-", value: "c" },
    ]);
    expect(result).toEqual(["a", "b", "c"]);
  });

  it("ADD by numeric index into an array", () => {
    const result = applyOps([1, 2, 3], [{ op: "UPDATE", path: "/1", value: 99 }]);
    expect(result).toEqual([1, 99, 3]);
  });

  it("REMOVE by numeric index splices the array", () => {
    const result = applyOps([1, 2, 3], [{ op: "REMOVE", path: "/1" }]);
    expect(result).toEqual([1, 3]);
  });

  // ── Unknown op type ─────────────────────────────────────────────────────────

  it("throws on unknown op type (lowercase 'add')", () => {
    expect(() =>
      applyOps({ x: 1 }, [{ op: "add" as unknown as "ADD", path: "/x", value: 99 }]),
    ).toThrow(/unknown delta op type/);
  });

  it("throws on completely unknown op type", () => {
    expect(() =>
      applyOps({ x: 1 }, [{ op: "FOO" as unknown as "ADD", path: "/x", value: 99 }]),
    ).toThrow(/unknown delta op type/);
  });
});
