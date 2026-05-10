import { describe, it, expect, beforeEach, afterEach } from '@jest/globals'
import { SodpServer } from './server'
import { StateStore } from './store'
import { AclRegistry } from './acl'
import { applyOps, diffShallow } from './jsonpointer'

// ── StateStore ────────────────────────────────────────────────────────────────

describe('StateStore', () => {
  let store: StateStore

  beforeEach(() => { store = new StateStore() })

  it('returns initialized:false for unknown keys', () => {
    expect(store.snapshot('missing')).toEqual({ version: 0, value: null, initialized: false })
  })

  it('hydrate seeds value without bumping version', () => {
    store.hydrate('k', { x: 1 })
    const snap = store.snapshot('k')
    expect(snap.initialized).toBe(true)
    expect(snap.version).toBe(0)
    expect(snap.value).toEqual({ x: 1 })
  })

  it('publish applies ops, bumps version, notifies subscribers', () => {
    const received: unknown[] = []
    store.subscribe('k', (d) => received.push(d.ops))
    store.publish('k', [{ op: 'UPDATE', path: '/', value: { a: 1 } }])
    expect(store.snapshot('k').version).toBe(1)
    expect(received).toHaveLength(1)
  })

  it('deltasSince returns null when sinceVersion is older than the log window', () => {
    // Fill the store with a small capacity so old entries are evicted.
    const small = new StateStore(2)
    small.publish('k', [{ op: 'UPDATE', path: '/', value: 1 }]) // v1
    small.publish('k', [{ op: 'UPDATE', path: '/', value: 2 }]) // v2
    small.publish('k', [{ op: 'UPDATE', path: '/', value: 3 }]) // v3 — v1 evicted
    // sinceVersion=0 is older than the oldest buffered delta (v2), so null.
    expect(small.deltasSince('k', 0)).toBeNull()
  })

  it('deltasSince returns entries newer than sinceVersion', () => {
    store.publish('k', [{ op: 'UPDATE', path: '/', value: 1 }])
    store.publish('k', [{ op: 'UPDATE', path: '/', value: 2 }])
    const deltas = store.deltasSince('k', 1)
    expect(deltas).toHaveLength(1)
    expect(deltas![0].version).toBe(2)
  })

  it('remove broadcasts REMOVE op and resets entry', () => {
    const ops: unknown[] = []
    store.hydrate('k', { x: 1 })
    store.subscribe('k', (d) => ops.push(d.ops))
    store.remove('k')
    expect(ops[0]).toEqual([{ op: 'REMOVE', path: '/' }])
  })
})

// ── JSON Pointer + diffShallow ─────────────────────────────────────────────────

describe('applyOps', () => {
  it('applies ADD to array via /-', () => {
    const result = applyOps([], [{ op: 'ADD', path: '/-', value: 'x' }])
    expect(result).toEqual(['x'])
  })

  it('creates array from null on array-like first segment', () => {
    const result = applyOps(null, [{ op: 'ADD', path: '/-', value: 1 }])
    expect(result).toEqual([1])
  })

  it('UPDATE on root replaces value', () => {
    expect(applyOps({ a: 1 }, [{ op: 'UPDATE', path: '/', value: { b: 2 } }])).toEqual({ b: 2 })
  })

  it('REMOVE on root returns null', () => {
    expect(applyOps({ a: 1 }, [{ op: 'REMOVE', path: '/' }])).toBeNull()
  })

  it('UPDATE nested field', () => {
    const result = applyOps({ a: { b: 1 } }, [{ op: 'UPDATE', path: '/a/b', value: 99 }])
    expect(result).toEqual({ a: { b: 99 } })
  })
})

describe('diffShallow', () => {
  it('returns [] when objects are identical', () => {
    const obj = { a: 1 }
    expect(diffShallow(obj, obj)).toEqual([])
  })

  it('detects ADD, UPDATE, REMOVE between plain objects', () => {
    const ops = diffShallow({ a: 1, b: 2 }, { b: 3, c: 4 })
    const opMap = Object.fromEntries(ops.map((o) => [o.path, o]))
    expect(opMap['/a'].op).toBe('REMOVE')
    expect(opMap['/b'].op).toBe('UPDATE')
    expect(opMap['/c'].op).toBe('ADD')
  })

  it('falls back to root UPDATE when types differ', () => {
    const ops = diffShallow([1, 2], [1, 2, 3])
    expect(ops).toEqual([{ op: 'UPDATE', path: '/', value: [1, 2, 3] }])
  })
})

// ── AclRegistry ───────────────────────────────────────────────────────────────

describe('AclRegistry', () => {
  const claims = { sub: 'alice', roles: ['editor'], groups: [], perms: [], extra: {} }
  const anon = { sub: '', roles: [], groups: [], perms: [], extra: {} }

  it('allows all when no rules match (permissive default)', () => {
    const acl = new AclRegistry([])
    expect(acl.canRead('anything', claims)).toBe(true)
  })

  it('matches literal pattern', () => {
    const acl = new AclRegistry([{ pattern: 'public.scores', read: ['*'], write: ['role:editor'] }])
    expect(acl.canRead('public.scores', anon)).toBe(true)
    expect(acl.canWrite('public.scores', anon)).toBe(false)
    expect(acl.canWrite('public.scores', claims)).toBe(true)
  })

  it('matches {sub} capture and enforces ownership', () => {
    const acl = new AclRegistry([{ pattern: 'user.{sub}', read: ['{sub}'], write: ['{sub}'] }])
    expect(acl.canRead('user.alice', claims)).toBe(true)
    expect(acl.canRead('user.bob', claims)).toBe(false)
  })

  it('matches wildcard suffix', () => {
    const acl = new AclRegistry([{ pattern: 'admin.*', read: ['role:admin'], write: ['role:admin'] }])
    expect(acl.canRead('admin.dashboard', { ...claims, roles: ['admin'] })).toBe(true)
    expect(acl.canRead('admin.dashboard', claims)).toBe(false)
    expect(acl.canRead('admin.deep.nested', { ...claims, roles: ['admin'] })).toBe(true)
  })
})

// ── SodpServer mutations ───────────────────────────────────────────────────────

describe('SodpServer server-side mutations', () => {
  let server: SodpServer

  beforeEach(() => {
    server = new SodpServer({ authRequired: false })
  })

  it('set() publishes a shallow diff', () => {
    server.set('k', { a: 1, b: 2 })
    server.set('k', { a: 1, b: 3, c: 4 })
    const snap = server.store.snapshot('k')
    expect(snap.value).toEqual({ a: 1, b: 3, c: 4 })
    expect(snap.version).toBe(2)
  })

  it('patch() merges fields', () => {
    server.set('k', { a: 1, b: 2 })
    server.patch('k', { b: 99, c: 3 })
    expect(server.store.snapshot('k').value).toEqual({ a: 1, b: 99, c: 3 })
  })

  it('setIn() updates nested field', () => {
    server.set('k', { pos: { x: 0, y: 0 } })
    server.setIn('k', '/pos/x', 42)
    expect((server.store.snapshot('k').value as Record<string, unknown>)['pos']).toEqual({ x: 42, y: 0 })
  })

  it('delete() removes the key', () => {
    server.set('k', { v: 1 })
    server.delete('k')
    expect(server.store.snapshot('k').initialized).toBe(false)
  })

  it('append() adds items to an array key', () => {
    server.append('events', 'a')
    server.append('events', 'b')
    server.append('events', 'c')
    expect(server.store.snapshot('events').value).toEqual(['a', 'b', 'c'])
  })

  it('append() with maxLen caps the array', () => {
    server.append('events', 'a')
    server.append('events', 'b')
    server.append('events', 'c', 2)
    expect(server.store.snapshot('events').value).toEqual(['b', 'c'])
  })
})
