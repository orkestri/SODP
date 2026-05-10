// Minimal RFC 6901 JSON Pointer + the three SODP delta ops (ADD, UPDATE, REMOVE).
// Applying ops to `null` state with a numeric or `-` first segment creates an
// array (not an object): applyOps(null, [ADD "/-", "x"]) → ["x"].

import type { DeltaOp } from './frame'

function parsePath(pointer: string): string[] {
  if (pointer === '' || pointer === '/') return []
  if (!pointer.startsWith('/')) throw new Error('pointer must start with /')
  return pointer
    .slice(1)
    .split('/')
    .map((seg) => seg.replace(/~1/g, '/').replace(/~0/g, '~'))
}

function isArrayLikeSegment(seg: string): boolean {
  return seg === '-' || /^(0|[1-9][0-9]*)$/.test(seg)
}

type Container = Record<string, unknown> | unknown[]

function seedIfNull(target: unknown, segments: string[]): unknown {
  if (target !== null) return target
  if (segments.length === 0) return null
  return isArrayLikeSegment(segments[0]) ? [] : {}
}

function cloneShallow(v: unknown): unknown {
  if (Array.isArray(v)) return v.slice()
  if (v && typeof v === 'object') return { ...(v as Record<string, unknown>) }
  return v
}

function applyOne(rootIn: unknown, op: DeltaOp): unknown {
  const segments = parsePath(op.path)

  if (segments.length === 0) {
    if (op.op === 'REMOVE') return null
    return op.value
  }

  let root = seedIfNull(rootIn, segments)
  root = cloneShallow(root)

  let parent = root as Container
  for (let i = 0; i < segments.length - 1; i++) {
    const seg = segments[i]
    if (Array.isArray(parent)) {
      const idx = seg === '-' ? parent.length - 1 : Number(seg)
      const raw = parent[idx]
      const next = raw === undefined || raw === null
        ? seedIfNull(null, segments.slice(i + 1))
        : cloneShallow(raw)
      parent[idx] = next as unknown
      parent = next as Container
    } else {
      const child = (parent as Record<string, unknown>)[seg]
      const next = child === undefined || child === null
        ? seedIfNull(null, segments.slice(i + 1))
        : cloneShallow(child)
      ;(parent as Record<string, unknown>)[seg] = next
      parent = next as Container
    }
  }

  const last = segments[segments.length - 1]

  if (Array.isArray(parent)) {
    if (last === '-') {
      if (op.op === 'ADD') {
        parent.push(op.value)
      } else {
        throw new Error(`cannot ${op.op} on array append path "-"`)
      }
    } else {
      const idx = Number(last)
      if (op.op === 'ADD') {
        parent.splice(idx, 0, op.value)
      } else if (op.op === 'UPDATE') {
        parent[idx] = op.value
      } else {
        parent.splice(idx, 1)
      }
    }
  } else {
    const obj = parent as Record<string, unknown>
    if (op.op === 'REMOVE') {
      delete obj[last]
    } else {
      obj[last] = op.value
    }
  }

  return root
}

export function applyOps(value: unknown, ops: DeltaOp[]): unknown {
  let current: unknown = value
  for (const op of ops) current = applyOne(current, op)
  return current
}

// Shallow diff: compares one level into objects, emits ADD/UPDATE/REMOVE per key.
// Falls back to a root UPDATE when either side is not a plain object.
export function diffShallow(before: unknown, after: unknown): DeltaOp[] {
  if (before === after) return []
  if (
    before && typeof before === 'object' && !Array.isArray(before) &&
    after  && typeof after  === 'object' && !Array.isArray(after)
  ) {
    const ops: DeltaOp[] = []
    const a = before as Record<string, unknown>
    const b = after  as Record<string, unknown>
    const keys = new Set([...Object.keys(a), ...Object.keys(b)])
    for (const k of keys) {
      const path = `/${k.replace(/~/g, '~0').replace(/\//g, '~1')}`
      if (!(k in a) && k in b) ops.push({ op: 'ADD', path, value: b[k] })
      else if (k in a && !(k in b)) ops.push({ op: 'REMOVE', path })
      else if (a[k] !== b[k]) ops.push({ op: 'UPDATE', path, value: b[k] })
    }
    return ops
  }
  return [{ op: 'UPDATE', path: '/', value: after }]
}
