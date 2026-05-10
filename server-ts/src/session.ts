// Per-connection SODP session.
//
// Owns stream-id allocation, outgoing sequence counter, subscription fan-out,
// heartbeat, and presence cleanup. All application-specific behaviour is
// injected via SessionConfig callbacks — no direct database imports here.

import type { WebSocket } from 'ws'
import type {
  Frame, WatchBody, UnwatchBody, AuthBody, ResumeBody,
} from './frame'
import { OP, Frames, decodeFrame } from './frame'
import type { StateStore, DeltaLogEntry } from './store'
import type { SodpClaims } from './jwt'

const HEARTBEAT_INTERVAL_MS = 30_000
const AUTH_TIMEOUT_MS = 15_000

export interface CallResult {
  success: boolean
  data: unknown
}

export interface SessionConfig {
  store: StateStore
  // Called during AUTH frame. Return claims on success, null to reject.
  authenticate: (token: string) => Promise<SodpClaims | null>
  // Called before every WATCH / RESUME. Return false → ERROR 403.
  authorize: (key: string, claims: SodpClaims) => Promise<boolean>
  // Called when a key is first watched and the store has no snapshot.
  // Return a value to seed initialized:true, or null for initialized:false.
  hydrate: (key: string) => Promise<unknown | null>
  // Called for every CALL frame.
  onCall: (method: string, args: Record<string, unknown>, claims: SodpClaims, session: SodpSession) => Promise<CallResult>
  // Whether auth is required before other ops.
  authRequired: boolean
  serverName: string
  rateLimitWrites: number
  rateLimitWatches: number
  backpressureLimit: number
}

interface Subscription {
  stream: number
  key: string
  unsubscribe: () => void
}

interface PresenceEntry {
  key: string
  path: string
}

class RateLimiter {
  private count = 0
  private window = Date.now()

  constructor(private readonly limit: number) {}

  check(): boolean {
    const now = Date.now()
    if (now - this.window >= 1000) {
      this.count = 0
      this.window = now
    }
    if (this.count >= this.limit) return false
    this.count++
    return true
  }
}

export class SodpSession {
  private readonly ws: WebSocket
  private readonly config: SessionConfig
  private outSeq = 0
  private claims: SodpClaims | null = null
  private nextStream = 10
  private readonly subs = new Map<string, Subscription>()
  private readonly presence: PresenceEntry[] = []
  private hbTimer: ReturnType<typeof setInterval> | null = null
  private hbMissed = false
  private authTimer: ReturnType<typeof setTimeout> | null = null
  private readonly writeLimiter: RateLimiter
  private readonly watchLimiter: RateLimiter

  constructor(ws: WebSocket, config: SessionConfig) {
    this.ws = ws
    this.config = config
    this.writeLimiter = new RateLimiter(config.rateLimitWrites)
    this.watchLimiter = new RateLimiter(config.rateLimitWatches)
  }

  start() {
    this.ws.on('message', (buf) => this.onMessage(buf))
    this.ws.on('close', () => this.teardown())
    this.ws.on('error', () => this.teardown())

    this.send(Frames.hello({
      protocol: 'sodp',
      version: '1.0',
      server: this.config.serverName,
      auth: this.config.authRequired,
      capabilities: {
        rate_limit_writes: this.config.rateLimitWrites,
        rate_limit_watches: this.config.rateLimitWatches,
        backpressure_limit: this.config.backpressureLimit,
        multi_watch: true,
        params: true,
        resume: true,
      },
    }))

    if (this.config.authRequired) {
      this.authTimer = setTimeout(() => {
        if (!this.claims) {
          this.sendError(0, 401, 'AUTH timeout')
          this.ws.close()
        }
      }, AUTH_TIMEOUT_MS)
    }

    this.hbTimer = setInterval(() => {
      if (this.hbMissed) {
        this.ws.close()
        return
      }
      this.hbMissed = true
      this.send(Frames.heartbeat())
    }, HEARTBEAT_INTERVAL_MS)
  }

  private async onMessage(buf: unknown) {
    let frame: Frame
    try {
      const bytes = buf instanceof Buffer ? buf
        : buf instanceof ArrayBuffer ? new Uint8Array(buf)
        : (buf as Uint8Array)
      frame = decodeFrame(bytes)
    } catch {
      this.sendError(0, 400, 'malformed frame')
      return
    }

    if (frame.op === OP.HEARTBEAT) {
      this.hbMissed = false
      return
    }

    if (frame.op === OP.AUTH) {
      await this.handleAuth(frame)
      return
    }

    if (this.config.authRequired && !this.claims) {
      this.sendError(0, 401, 'AUTH required')
      return
    }

    switch (frame.op) {
      case OP.WATCH:   await this.handleWatch(frame);  break
      case OP.UNWATCH: await this.handleUnwatch(frame); break
      case OP.RESUME:  await this.handleResume(frame); break
      case OP.CALL:    await this.handleCall(frame);   break
      default:
        this.sendError(0, 400, `unexpected op 0x${frame.op.toString(16)}`)
    }
  }

  private async handleAuth(frame: Frame) {
    const body = frame.body as AuthBody
    const token = typeof body?.token === 'string' ? body.token.trim() : ''
    if (!token) {
      this.sendError(0, 401, 'missing token')
      this.ws.close()
      return
    }

    const claims = await this.config.authenticate(token)
    if (!claims) {
      this.sendError(0, 401, 'invalid token')
      this.ws.close()
      return
    }

    this.claims = claims
    if (this.authTimer) { clearTimeout(this.authTimer); this.authTimer = null }
    this.send(Frames.authOk(frame.seq, { sub: claims.sub }))
  }

  private async handleWatch(frame: Frame) {
    if (!this.watchLimiter.check()) {
      this.sendError(0, 429, 'watch rate limit exceeded')
      return
    }
    const body = frame.body as WatchBody
    const keys = body.states ?? (body.state ? [body.state] : [])
    if (keys.length === 0) { this.sendError(0, 400, 'missing state(s)'); return }
    for (const key of keys) await this.watchKey(key, body.params, frame.seq)
  }

  private async watchKey(key: string, params: Record<string, unknown> | undefined, seq: number) {
    const claims = this.claims ?? (this.config.authRequired ? null : { sub: '', roles: [], groups: [], perms: [], extra: {} })
    if (claims && !await this.config.authorize(key, claims)) {
      this.sendError(0, 403, `access denied: ${key}`)
      return
    }

    const existing = this.subs.get(key)
    const stream = existing?.stream ?? this.nextStream++
    if (!existing) {
      const unsubscribe = this.config.store.subscribe(key, (delta, k) => this.onDelta(stream, delta, k))
      this.subs.set(key, { stream, key, unsubscribe })
    }

    await this.ensureHydrated(key)
    const snap = this.config.store.snapshot(key)
    this.send(Frames.stateInit(stream, seq, {
      state: key,
      version: snap.version,
      value: snap.value,
      initialized: snap.initialized,
      ...(params ? { params } : {}),
    }))
  }

  private async handleUnwatch(frame: Frame) {
    const body = frame.body as UnwatchBody
    const keys = body.states ?? (body.state ? [body.state] : [])
    for (const key of keys) {
      const sub = this.subs.get(key)
      if (!sub) continue
      sub.unsubscribe()
      this.subs.delete(key)
    }
  }

  private async handleResume(frame: Frame) {
    if (!this.watchLimiter.check()) {
      this.sendError(0, 429, 'watch rate limit exceeded')
      return
    }
    const body = frame.body as ResumeBody
    const { state: key, since_version, params } = body
    const claims = this.claims ?? (this.config.authRequired ? null : { sub: '', roles: [], groups: [], perms: [], extra: {} })
    if (claims && !await this.config.authorize(key, claims)) {
      this.sendError(0, 403, `access denied: ${key}`)
      return
    }

    const stream = this.subs.get(key)?.stream ?? this.nextStream++
    if (!this.subs.has(key)) {
      const unsubscribe = this.config.store.subscribe(key, (d, k) => this.onDelta(stream, d, k))
      this.subs.set(key, { stream, key, unsubscribe })
    }

    const deltas = this.config.store.deltasSince(key, since_version ?? 0)
    if (deltas) {
      for (const d of deltas) {
        this.send(Frames.delta(stream, this.nextSeq(), { version: d.version, ops: d.ops, state: key }))
      }
    }

    await this.ensureHydrated(key)
    const snap = this.config.store.snapshot(key)
    this.send(Frames.stateInit(stream, frame.seq, {
      state: key,
      version: snap.version,
      value: snap.value,
      initialized: snap.initialized,
      ...(params ? { params } : {}),
    }))
  }

  private async handleCall(frame: Frame) {
    if (!this.writeLimiter.check()) {
      this.sendError(0, 429, 'write rate limit exceeded')
      return
    }
    const body = frame.body as { call_id?: string; method?: string; args?: Record<string, unknown> }
    const callId = body?.call_id ?? ''
    const method = body?.method ?? ''
    const args = body?.args ?? {}

    const claims = this.claims ?? { sub: '', roles: [], groups: [], perms: [], extra: {} }
    try {
      const result = await this.config.onCall(method, args, claims, this)
      this.send(Frames.result(frame.seq, callId, result.success, result.data))
    } catch (err) {
      const msg = err instanceof Error ? err.message : 'internal error'
      this.send(Frames.result(frame.seq, callId, false, msg))
    }
  }

  private onDelta(stream: number, delta: DeltaLogEntry, key: string) {
    this.send(Frames.delta(stream, this.nextSeq(), {
      version: delta.version,
      ops: delta.ops,
      state: key,
    }))
  }

  private async ensureHydrated(key: string) {
    const snap = this.config.store.snapshot(key)
    if (snap.initialized) return
    try {
      const value = await this.config.hydrate(key)
      if (value !== null) this.config.store.hydrate(key, value)
    } catch { /* hydration failures are non-fatal */ }
  }

  // Called by RPC handler to register a presence entry for cleanup on disconnect.
  registerPresence(key: string, path: string) {
    this.presence.push({ key, path })
  }

  send(bytes: Uint8Array) {
    if (this.ws.readyState !== 1 /* OPEN */) return
    this.ws.send(bytes)
  }

  sendError(stream: number, code: number, message: string) {
    this.send(Frames.error(stream, this.nextSeq(), { code, message }))
  }

  nextSeq() { return ++this.outSeq }

  get currentClaims(): SodpClaims | null { return this.claims }

  private teardown() {
    if (this.hbTimer) clearInterval(this.hbTimer)
    if (this.authTimer) clearTimeout(this.authTimer)

    // Remove all presence entries this session registered.
    for (const { key, path } of this.presence) {
      try {
        this.config.store.publish(key, [{ op: 'REMOVE', path }])
      } catch { /* ignore */ }
    }

    for (const sub of this.subs.values()) sub.unsubscribe()
    this.subs.clear()
  }
}
