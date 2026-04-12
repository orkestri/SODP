// ── Delta op types ─────────────────────────────────────────────────────────────

export type DeltaOp =
  | { op: "ADD";    path: string; value: unknown }
  | { op: "UPDATE"; path: string; value: unknown }
  | { op: "REMOVE"; path: string };

// ── Deep clone helper ─────────────────────────────────────────────────────────

/** Deep clone that works on Node 16 and older browsers lacking structuredClone. */
function deepClone<T>(value: T): T {
  if (typeof globalThis.structuredClone === "function") {
    return globalThis.structuredClone(value);
  }
  return JSON.parse(JSON.stringify(value)) as T;
}

// ── Path parsing ───────────────────────────────────────────────────────────────

/**
 * Parse a JSON-pointer-style path into its segments.
 *
 * "/"         → []           (root)
 * "/x"        → ["x"]
 * "/x/y/z"   → ["x", "y", "z"]
 * "/-"        → ["-"]        (RFC 6901 array append token)
 */
function parsePath(path: string): string[] {
  if (path === "/") return [];
  return path.slice(1).split("/");
}

// ── Known op types ────────────────────────────────────────────────────────────

const KNOWN_OPS = new Set(["ADD", "UPDATE", "REMOVE"]);

// ── Op application ─────────────────────────────────────────────────────────────

/**
 * Apply a sequence of delta ops to `state` and return the new state value.
 * Does not mutate the input — clones before modification.
 *
 * Throws on unknown op types.
 */
export function applyOps(state: unknown, ops: DeltaOp[]): unknown {
  for (const op of ops) {
    state = applyOp(state, op);
  }
  return state;
}

function applyOp(state: unknown, op: DeltaOp): unknown {
  if (!KNOWN_OPS.has(op.op)) {
    throw new Error(`[SODP] unknown delta op type: "${op.op}". Expected one of: ADD, UPDATE, REMOVE`);
  }

  const parts = parsePath(op.path);

  // Root-level operation.
  if (parts.length === 0) {
    if (op.op === "REMOVE") return null;
    return (op as { value: unknown }).value;
  }

  // Clone so callers keep immutability guarantees.
  const root = (
    typeof state === "object" && state !== null
      ? deepClone(state)
      : {}
  ) as Record<string, unknown>;

  // Navigate to the parent node, creating intermediate objects as needed.
  let node: unknown = root;
  for (let i = 0; i < parts.length - 1; i++) {
    const key = parts[i];
    const parent = node as Record<string, unknown>;
    if (typeof parent[key] !== "object" || parent[key] === null) {
      parent[key] = {};
    }
    node = parent[key];
  }

  const last = parts[parts.length - 1];

  // RFC 6901: "-" references the element past the end of an array (append).
  if (last === "-" && Array.isArray(node)) {
    switch (op.op) {
      case "ADD":
      case "UPDATE":
        (node as unknown[]).push((op as { value: unknown }).value);
        break;
      case "REMOVE":
        // Remove last element when targeting "-" on a REMOVE
        (node as unknown[]).pop();
        break;
    }
    return root;
  }

  const parent = node as Record<string, unknown>;
  switch (op.op) {
    case "ADD":
    case "UPDATE":
      // If parent is an array and key is numeric, set by index.
      if (Array.isArray(parent)) {
        const idx = Number(last);
        if (!Number.isNaN(idx)) {
          (parent as unknown[])[idx] = (op as { value: unknown }).value;
          break;
        }
      }
      parent[last] = (op as { value: unknown }).value;
      break;
    case "REMOVE":
      if (Array.isArray(parent)) {
        const idx = Number(last);
        if (!Number.isNaN(idx)) {
          (parent as unknown[]).splice(idx, 1);
          break;
        }
      }
      delete parent[last];
      break;
  }

  return root;
}
