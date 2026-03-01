// ── Delta op types ─────────────────────────────────────────────────────────────

export type DeltaOp =
  | { op: "ADD";    path: string; value: unknown }
  | { op: "UPDATE"; path: string; value: unknown }
  | { op: "REMOVE"; path: string };

// ── Path parsing ───────────────────────────────────────────────────────────────

/**
 * Parse a JSON-pointer-style path into its segments.
 *
 * "/"         → []           (root)
 * "/x"        → ["x"]
 * "/x/y/z"   → ["x", "y", "z"]
 */
function parsePath(path: string): string[] {
  if (path === "/") return [];
  return path.slice(1).split("/");
}

// ── Op application ─────────────────────────────────────────────────────────────

/**
 * Apply a sequence of delta ops to `state` and return the new state value.
 * Does not mutate the input — uses structuredClone per op.
 */
export function applyOps(state: unknown, ops: DeltaOp[]): unknown {
  for (const op of ops) {
    state = applyOp(state, op);
  }
  return state;
}

function applyOp(state: unknown, op: DeltaOp): unknown {
  const parts = parsePath(op.path);

  // Root-level operation.
  if (parts.length === 0) {
    if (op.op === "REMOVE") return null;
    return (op as { value: unknown }).value;
  }

  // Clone so callers keep immutability guarantees.
  const root: Record<string, unknown> = (
    typeof state === "object" && state !== null
      ? structuredClone(state)
      : {}
  ) as Record<string, unknown>;

  // Navigate to the parent node, creating intermediate objects as needed.
  let node: Record<string, unknown> = root;
  for (let i = 0; i < parts.length - 1; i++) {
    const key = parts[i];
    if (typeof node[key] !== "object" || node[key] === null) {
      node[key] = {};
    }
    node = node[key] as Record<string, unknown>;
  }

  const last = parts[parts.length - 1];
  switch (op.op) {
    case "ADD":
    case "UPDATE":
      node[last] = (op as { value: unknown }).value;
      break;
    case "REMOVE":
      delete node[last];
      break;
  }

  return root;
}
