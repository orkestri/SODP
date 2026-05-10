// Protocol correctness tests — real WebSocket connections to a real SodpServer.
// Each test gets a fresh server on a random OS-assigned port (port 0) and
// connects actual WebSocket clients. This validates the full frame flow, not
// just unit logic.

import { describe, it, expect, beforeEach, afterEach } from '@jest/globals'
import WebSocket from 'ws'
import { encode } from '@msgpack/msgpack'
import { SodpServer } from './server'
import type { SodpClaims } from './jwt'
import { decodeFrame, OP } from './frame'
import type { Frame } from './frame'

// ── TestClient ────────────────────────────────────────────────────────────────

class TestClient {
  ws: WebSocket
  private queue: Frame[] = []
  private waiters: Array<(f: Frame) => void> = []
  closed = false
  closeCode: number | undefined

  constructor(port: number) {
    this.ws = new WebSocket(`ws://127.0.0.1:${port}`)
    this.ws.binaryType = 'arraybuffer'
    this.ws.on('message', (data) => {
      const frame = decodeFrame(data as ArrayBuffer)
      const waiter = this.waiters.shift()
      if (waiter) waiter(frame)
      else this.queue.push(frame)
    })
    this.ws.on('close', (code) => {
      this.closed = true
      this.closeCode = code
      // Drain all pending waiters with a sentinel — avoids hanging.
      for (const w of this.waiters) w({ op: 0x00 as never, stream: 0, seq: 0, body: null })
      this.waiters = []
    })
  }

  connected(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.ws.once('open', resolve)
      this.ws.once('error', reject)
    })
  }

  recv(timeoutMs = 2000): Promise<Frame> {
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift()!)
    return new Promise((resolve, reject) => {
      let settled = false
      const timer = setTimeout(() => {
        if (settled) return
        settled = true
        const idx = this.waiters.findIndex((w) => w === waiter)
        if (idx !== -1) this.waiters.splice(idx, 1)
        reject(new Error('recv timeout'))
      }, timeoutMs)
      const waiter = (f: Frame) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        resolve(f)
      }
      this.waiters.push(waiter)
    })
  }

  // Wait until the connection is closed (with timeout).
  waitClose(timeoutMs = 2000): Promise<void> {
    if (this.closed) return Promise.resolve()
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('waitClose timeout')), timeoutMs)
      this.ws.once('close', () => { clearTimeout(timer); resolve() })
    })
  }

  raw(op: number, stream: number, seq: number, body: unknown): void {
    this.ws.send(encode([op, stream, seq, body]))
  }

  auth(token: string, seq = 1): void {
    this.raw(OP.AUTH, 0, seq, { token })
  }

  watch(key: string, seq = 2): void {
    this.raw(OP.WATCH, 0, seq, { state: key })
  }

  watchMany(keys: string[], seq = 2): void {
    this.raw(OP.WATCH, 0, seq, { states: keys })
  }

  unwatch(key: string, seq = 4): void {
    this.raw(OP.UNWATCH, 0, seq, { state: key })
  }

  resume(key: string, sinceVersion: number, seq = 2): void {
    this.raw(OP.RESUME, 0, seq, { state: key, since_version: sinceVersion })
  }

  call(method: string, args: Record<string, unknown>, seq = 3): void {
    this.raw(OP.CALL, 0, seq, { call_id: `cid-${seq}`, method, args })
  }

  heartbeat(): void {
    this.raw(OP.HEARTBEAT, 0, 0, null)
  }

  close(): void { this.ws.close() }
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

function makeClaims(sub: string): SodpClaims {
  return { sub, roles: [], groups: [], perms: [], extra: {} }
}

// Test auth: "valid:<sub>" accepted, everything else rejected.
const testAuthenticate = (token: string): Promise<SodpClaims | null> =>
  Promise.resolve(token.startsWith('valid:') ? makeClaims(token.slice(6)) : null)

async function openClient(port: number): Promise<TestClient> {
  const c = new TestClient(port)
  await c.connected()
  return c
}

// Helpers to connect + consume HELLO (most tests don't care about HELLO).
async function authedClient(port: number, sub = 'alice'): Promise<TestClient> {
  const c = await openClient(port)
  await c.recv() // HELLO
  c.auth(`valid:${sub}`)
  const frame = await c.recv()
  expect(frame.op).toBe(OP.AUTH_OK)
  return c
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('HELLO', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('sends HELLO immediately on connect', async () => {
    const c = await openClient(port)
    const f = await c.recv()
    expect(f.op).toBe(OP.HELLO)
    const body = f.body as Record<string, unknown>
    expect(body['protocol']).toBe('sodp')
    expect(body['version']).toBe('1.0')
    expect(body['capabilities']).toBeDefined()
    c.close()
  })

  it('HELLO capabilities advertise correct flags', async () => {
    const c = await openClient(port)
    const f = await c.recv()
    const caps = (f.body as Record<string, unknown>)['capabilities'] as Record<string, unknown>
    expect(caps['multi_watch']).toBe(true)
    expect(caps['params']).toBe(true)
    expect(caps['resume']).toBe(true)
    c.close()
  })
})

describe('AUTH', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: true, authenticate: testAuthenticate })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('accepts a valid token and sends AUTH_OK with sub', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.auth('valid:alice')
    const f = await c.recv()
    expect(f.op).toBe(OP.AUTH_OK)
    expect((f.body as Record<string, unknown>)['sub']).toBe('alice')
    c.close()
  })

  it('rejects an invalid token with ERROR 401 and closes the connection', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.auth('bad-token')
    const f = await c.recv()
    expect(f.op).toBe(OP.ERROR)
    expect((f.body as Record<string, unknown>)['code']).toBe(401)
    await c.waitClose()
    expect(c.closed).toBe(true)
  })

  it('rejects WATCH before AUTH with ERROR 401', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('game.score')
    const f = await c.recv()
    expect(f.op).toBe(OP.ERROR)
    expect((f.body as Record<string, unknown>)['code']).toBe(401)
    c.close()
  })
})

describe('WATCH', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('sends STATE_INIT { initialized: false } for an unknown key', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('fresh.key')
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    const body = f.body as Record<string, unknown>
    expect(body['initialized']).toBe(false)
    expect(body['value']).toBeNull()
    expect(body['state']).toBe('fresh.key')
    c.close()
  })

  it('sends STATE_INIT { initialized: true } for a pre-seeded key', async () => {
    server.set('game.score', { value: 99 })
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('game.score')
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    const body = f.body as Record<string, unknown>
    expect(body['initialized']).toBe(true)
    expect((body['value'] as Record<string, unknown>)['value']).toBe(99)
    c.close()
  })

  it('second WATCH on same key sends a fresh STATE_INIT without double-subscribing', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('k')
    await c.recv() // first STATE_INIT

    c.watch('k') // second watch — idempotent
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)

    // Only one DELTA should arrive per mutation (not doubled).
    server.set('k', { x: 1 })
    const d1 = await c.recv()
    expect(d1.op).toBe(OP.DELTA)

    // No second DELTA should arrive — queue should be empty.
    let extra: Frame | null = null
    try { extra = await c.recv(100) } catch { /* timeout = expected */ }
    expect(extra).toBeNull()
    c.close()
  })

  it('watches multiple keys via states array', async () => {
    server.set('a', 1)
    server.set('b', 2)
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watchMany(['a', 'b'])
    const f1 = await c.recv()
    const f2 = await c.recv()
    expect(f1.op).toBe(OP.STATE_INIT)
    expect(f2.op).toBe(OP.STATE_INIT)
    const states = new Set([
      (f1.body as Record<string, unknown>)['state'],
      (f2.body as Record<string, unknown>)['state'],
    ])
    expect(states).toEqual(new Set(['a', 'b']))
    c.close()
  })

  it('sends params back in STATE_INIT when provided in WATCH', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.raw(OP.WATCH, 0, 2, { state: 'k', params: { filter: 'active' } })
    const f = await c.recv()
    expect((f.body as Record<string, unknown>)['params']).toEqual({ filter: 'active' })
    c.close()
  })
})

describe('CALL — state mutations', () => {
  let server: SodpServer
  let port: number
  let watcher: TestClient

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
    watcher = await openClient(port)
    await watcher.recv() // HELLO
    watcher.watch('k')
    await watcher.recv() // STATE_INIT
  })
  afterEach(async () => {
    watcher.close()
    await server.close()
  })

  it('state.set broadcasts DELTA to watchers', async () => {
    const writer = await openClient(port)
    await writer.recv() // HELLO
    writer.call('state.set', { state: 'k', value: { x: 1 } })
    const result = await writer.recv()
    expect(result.op).toBe(OP.RESULT)
    expect((result.body as Record<string, unknown>)['success']).toBe(true)

    const delta = await watcher.recv()
    expect(delta.op).toBe(OP.DELTA)
    const ops = (delta.body as Record<string, unknown>)['ops'] as unknown[]
    expect(ops.length).toBeGreaterThan(0)
    writer.close()
  })

  it('state.patch emits only changed fields', async () => {
    server.set('k', { a: 1, b: 2 })
    await watcher.recv() // DELTA from server.set — flush it

    const writer = await openClient(port)
    await writer.recv() // HELLO
    writer.call('state.patch', { state: 'k', patch: { b: 99 } })
    await writer.recv() // RESULT

    const delta = await watcher.recv()
    const ops = (delta.body as Record<string, unknown>)['ops'] as Array<Record<string, unknown>>
    expect(ops).toHaveLength(1)
    expect(ops[0]!['path']).toBe('/b')
    expect(ops[0]!['value']).toBe(99)
    writer.close()
  })

  it('state.set_in updates a nested field via JSON Pointer', async () => {
    server.set('k', { pos: { x: 0, y: 0 } })
    await watcher.recv() // DELTA

    const writer = await openClient(port)
    await writer.recv() // HELLO
    writer.call('state.set_in', { state: 'k', path: '/pos/x', value: 42 })
    await writer.recv() // RESULT

    const delta = await watcher.recv()
    const ops = (delta.body as Record<string, unknown>)['ops'] as Array<Record<string, unknown>>
    expect(ops[0]!['path']).toBe('/pos/x')
    expect(ops[0]!['value']).toBe(42)
    writer.close()
  })

  it('state.delete broadcasts a REMOVE op', async () => {
    server.set('k', { v: 1 })
    await watcher.recv() // DELTA

    const writer = await openClient(port)
    await writer.recv() // HELLO
    writer.call('state.delete', { state: 'k' })
    await writer.recv() // RESULT

    const delta = await watcher.recv()
    const ops = (delta.body as Record<string, unknown>)['ops'] as Array<Record<string, unknown>>
    expect(ops[0]!['op']).toBe('REMOVE')
    writer.close()
  })

  it('state.presence sets a value and removes it on disconnect', async () => {
    const presenceWatcher = await openClient(port)
    await presenceWatcher.recv() // HELLO
    presenceWatcher.watch('collab.cursors')
    await presenceWatcher.recv() // STATE_INIT

    const user = await openClient(port)
    await user.recv() // HELLO
    user.call('state.presence', { state: 'collab.cursors', path: '/alice', value: { x: 10 } })
    await user.recv() // RESULT

    // Presence watcher should receive the ADD delta.
    const addDelta = await presenceWatcher.recv()
    expect(addDelta.op).toBe(OP.DELTA)

    // When the user disconnects, a REMOVE delta should be sent automatically.
    user.close()
    const removeDelta = await presenceWatcher.recv(3000)
    const ops = (removeDelta.body as Record<string, unknown>)['ops'] as Array<Record<string, unknown>>
    expect(ops[0]!['op']).toBe('REMOVE')
    expect(ops[0]!['path']).toBe('/alice')
    presenceWatcher.close()
  })
})

describe('UNWATCH', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('stops DELTA delivery after UNWATCH', async () => {
    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('k')
    await c.recv() // STATE_INIT

    c.unwatch('k')
    // Allow the unwatch to process.
    await new Promise((r) => setTimeout(r, 50))

    server.set('k', { v: 1 })

    let extra: Frame | null = null
    try { extra = await c.recv(150) } catch { /* timeout = expected */ }
    expect(extra).toBeNull()
    c.close()
  })
})

describe('RESUME', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('replays missed deltas then sends STATE_INIT as live marker', async () => {
    server.set('k', { v: 1 }) // version 1
    server.set('k', { v: 2 }) // version 2
    server.set('k', { v: 3 }) // version 3

    const c = await openClient(port)
    await c.recv() // HELLO
    // Resume from version 1 — expect deltas for v2 and v3, then STATE_INIT.
    c.resume('k', 1)

    const d1 = await c.recv()
    const d2 = await c.recv()
    const init = await c.recv()

    expect(d1.op).toBe(OP.DELTA)
    expect(d2.op).toBe(OP.DELTA)
    expect(init.op).toBe(OP.STATE_INIT)
    expect((init.body as Record<string, unknown>)['initialized']).toBe(true)
    c.close()
  })

  it('falls back to STATE_INIT when log window cannot cover sinceVersion', async () => {
    // Use a tiny capacity store so the log evicts early.
    const tinyStore = new (await import('./store')).StateStore(1)
    const s = new SodpServer({ authRequired: false, store: tinyStore })
    await s.listen(0)
    const p = s.address!.port

    s.set('k', { v: 1 }) // v1 — will be evicted
    s.set('k', { v: 2 }) // v2 — oldest in log now

    const c = await openClient(p)
    await c.recv() // HELLO
    c.resume('k', 0) // sinceVersion=0 is older than what's in the log

    // Should get STATE_INIT directly (no delta replay possible).
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    c.close()
    await s.close()
  })
})

describe('fan-out', () => {
  let server: SodpServer
  let port: number

  beforeEach(async () => {
    server = new SodpServer({ authRequired: false })
    await server.listen(0)
    port = server.address!.port
  })
  afterEach(() => server.close())

  it('delivers the same DELTA to all watchers', async () => {
    const N = 5
    const clients = await Promise.all(
      Array.from({ length: N }, () => openClient(port))
    )
    // Drain HELLOs and subscribe each client.
    for (const c of clients) {
      await c.recv() // HELLO
      c.watch('shared')
      await c.recv() // STATE_INIT
    }

    server.set('shared', { tick: 1 })

    const deltas = await Promise.all(clients.map((c) => c.recv()))
    for (const d of deltas) {
      expect(d.op).toBe(OP.DELTA)
      const ops = (d.body as Record<string, unknown>)['ops'] as Array<Record<string, unknown>>
      expect(ops[0]!['value']).toEqual({ tick: 1 })
    }

    for (const c of clients) c.close()
  })
})

describe('ACL', () => {
  it('denies WATCH when canRead returns false', async () => {
    const server = new SodpServer({
      authRequired: false,
      authorize: (key) => Promise.resolve(key !== 'secret'),
    })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('secret')
    const f = await c.recv()
    expect(f.op).toBe(OP.ERROR)
    expect((f.body as Record<string, unknown>)['code']).toBe(403)
    c.close()
    await server.close()
  })

  it('allows WATCH on permitted keys', async () => {
    const server = new SodpServer({
      authRequired: false,
      authorize: (key) => Promise.resolve(key === 'public'),
    })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('public')
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    c.close()
    await server.close()
  })
})

describe('authRequired=false', () => {
  it('allows WATCH without AUTH frame', async () => {
    const server = new SodpServer({ authRequired: false })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('open.key') // no AUTH sent
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    c.close()
    await server.close()
  })
})

describe('HEARTBEAT', () => {
  it('client heartbeat keeps connection alive', async () => {
    const server = new SodpServer({ authRequired: false })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO

    c.heartbeat()
    // Small delay then verify the connection is still open.
    await new Promise((r) => setTimeout(r, 100))
    expect(c.closed).toBe(false)
    c.close()
    await server.close()
  })
})

describe('rate limiting', () => {
  it('sends ERROR 429 when watch rate is exceeded', async () => {
    const server = new SodpServer({ authRequired: false, rateLimitWatches: 3 })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO

    // Exhaust the rate limit in one second window.
    for (let i = 0; i < 3; i++) {
      c.watch(`key${i}`)
      await c.recv() // STATE_INIT
    }

    // One more should hit the limit.
    c.watch('overflow')
    const f = await c.recv()
    expect(f.op).toBe(OP.ERROR)
    expect((f.body as Record<string, unknown>)['code']).toBe(429)
    c.close()
    await server.close()
  })

  it('sends ERROR 429 when write rate is exceeded', async () => {
    const server = new SodpServer({ authRequired: false, rateLimitWrites: 2 })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO

    for (let i = 0; i < 2; i++) {
      c.call('state.set', { state: 'k', value: i })
      await c.recv() // RESULT
    }

    c.call('state.set', { state: 'k', value: 99 })
    const f = await c.recv()
    expect(f.op).toBe(OP.ERROR)
    expect((f.body as Record<string, unknown>)['code']).toBe(429)
    c.close()
    await server.close()
  })
})

describe('server-side hydration hook', () => {
  it('calls hydrate when key is first watched', async () => {
    let called = false
    const server = new SodpServer({
      authRequired: false,
      hydrate: async (key) => {
        called = true
        return key === 'db.item' ? { id: 42, name: 'Widget' } : null
      },
    })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO
    c.watch('db.item')
    const f = await c.recv()
    expect(f.op).toBe(OP.STATE_INIT)
    expect(called).toBe(true)
    const body = f.body as Record<string, unknown>
    expect(body['initialized']).toBe(true)
    expect((body['value'] as Record<string, unknown>)['name']).toBe('Widget')
    c.close()
    await server.close()
  })
})

describe('custom onCall hook', () => {
  it('routes unknown methods to the application onCall handler', async () => {
    const server = new SodpServer({
      authRequired: false,
      onCall: async (method, args) => {
        if (method === 'app.echo') return { success: true, data: args['msg'] }
        return { success: false, data: 'unknown' }
      },
    })
    await server.listen(0)
    const port = server.address!.port

    const c = await openClient(port)
    await c.recv() // HELLO
    c.call('app.echo', { msg: 'hello' })
    const f = await c.recv()
    expect(f.op).toBe(OP.RESULT)
    expect((f.body as Record<string, unknown>)['data']).toBe('hello')
    c.close()
    await server.close()
  })
})
