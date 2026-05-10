// SodpServer — the main entry point for embedding a SODP server in a Node.js
// application.
//
// Usage — attach to an existing HTTP server:
//   const server = new SodpServer({ jwtSecret: process.env.SODP_JWT_SECRET })
//   server.attach(httpServer, { path: '/sodp' })
//
// Usage — standalone:
//   await server.listen(7777)
//
// Server-side mutations (fan-out to all watchers):
//   server.set('game.score', { value: 42 })
//   server.patch('game.player', { health: 80 })
//   server.setIn('game.player', '/position/x', 5)
//   server.delete('game.score')
//   server.append('game.events', event, 500)   // 500 = max array length
//   server.mutate('game.player', ops)

import { WebSocketServer, type WebSocket } from 'ws'
import type http from 'http'
import type https from 'https'
import type { AddressInfo } from 'net'
import { StateStore } from './store'
import { SodpSession, type CallResult } from './session'
import { AclRegistry } from './acl'
import { verifyJwt, jwtConfigFromEnv, type SodpClaims, type JwtConfig } from './jwt'
import { handleBuiltinCall } from './rpc'
import { diffShallow } from './jsonpointer'
import type { DeltaOp } from './frame'

export type { SodpClaims, JwtConfig } from './jwt'
export type { AclRule } from './acl'
export type { DeltaOp } from './frame'
export type { DeltaLogEntry } from './store'

export interface SodpServerOptions {
  // JWT auth — pick one, or provide a custom authenticate hook.
  // If none is set, auth is disabled (authRequired defaults to false).
  jwtSecret?: string
  jwtPublicKey?: string       // RS256 public key PEM
  jwtConfig?: JwtConfig       // pre-built config, overrides the above

  // Custom auth hook — overrides JWT if provided.
  // Return SodpClaims on success, null to reject with 401.
  authenticate?: (token: string) => Promise<SodpClaims | null>

  // ACL — pick one.
  aclFile?: string
  acl?: AclRegistry

  // Custom ACL hook — overrides aclFile if provided.
  // Return false to send ERROR 403.
  authorize?: (key: string, claims: SodpClaims) => Promise<boolean>

  // Hydration hook — called when a key is first watched and the store has no
  // snapshot. Return a value to seed initialized:true, or null/undefined to
  // leave the key as initialized:false.
  hydrate?: (key: string) => Promise<unknown | null>

  // Custom CALL handler for application-specific methods.
  // Standard state.* methods are handled automatically before this is called.
  onCall?: (method: string, args: Record<string, unknown>, claims: SodpClaims, session: SodpSession) => Promise<CallResult>

  // Whether AUTH frame is required. Defaults to true when any JWT config or
  // custom authenticate hook is set, false otherwise.
  authRequired?: boolean

  // Operational limits (env vars SODP_BACKPRESSURE_LIMIT etc. used as fallback).
  backpressureLimit?: number
  rateLimitWrites?: number
  rateLimitWatches?: number

  // Identifies the server in HELLO frames.
  serverName?: string

  // Provide a custom store (useful for testing or shared state across multiple servers).
  store?: StateStore
}

export class SodpServer {
  readonly store: StateStore
  private wss: WebSocketServer | null = null
  private readonly opts: Required<Omit<SodpServerOptions, 'jwtSecret' | 'jwtPublicKey' | 'jwtConfig' | 'authenticate' | 'aclFile' | 'acl' | 'authorize' | 'hydrate' | 'onCall' | 'store'>>
  private readonly resolvedAuthenticate: (token: string) => Promise<SodpClaims | null>
  private readonly resolvedAuthorize: (key: string, claims: SodpClaims) => Promise<boolean>
  private readonly resolvedHydrate: (key: string) => Promise<unknown | null>
  private readonly resolvedOnCall: (method: string, args: Record<string, unknown>, claims: SodpClaims, session: SodpSession) => Promise<CallResult>
  private readonly aclRegistry: AclRegistry | null

  constructor(options: SodpServerOptions = {}) {
    this.store = options.store ?? new StateStore()

    const backpressureLimit = options.backpressureLimit
      ?? parseInt(process.env['SODP_BACKPRESSURE_LIMIT'] ?? '1024', 10)
    const rateLimitWrites = options.rateLimitWrites
      ?? parseInt(process.env['SODP_RATE_WRITES_PER_SEC'] ?? '50', 10)
    const rateLimitWatches = options.rateLimitWatches
      ?? parseInt(process.env['SODP_RATE_WATCHES_PER_SEC'] ?? '10', 10)

    this.opts = {
      authRequired: options.authRequired ?? this.hasAuthConfig(options),
      backpressureLimit,
      rateLimitWrites,
      rateLimitWatches,
      serverName: options.serverName ?? 'sodp-server-ts',
    }

    // Build JWT config from options or env vars.
    const jwtCfg: JwtConfig | null =
      options.jwtConfig ??
      (options.jwtSecret ? { type: 'hs256', secret: options.jwtSecret } : null) ??
      (options.jwtPublicKey ? { type: 'rs256', publicKeyPem: options.jwtPublicKey } : null) ??
      jwtConfigFromEnv()

    // Auth
    if (options.authenticate) {
      this.resolvedAuthenticate = options.authenticate
    } else if (jwtCfg) {
      this.resolvedAuthenticate = (token) => verifyJwt(token, jwtCfg).catch(() => null)
    } else {
      // Auth disabled — return empty claims for any token.
      this.resolvedAuthenticate = (token) =>
        Promise.resolve(token ? { sub: token, roles: [], groups: [], perms: [], extra: {} } : null)
    }

    // ACL
    if (options.acl) {
      this.aclRegistry = options.acl
    } else if (options.aclFile) {
      this.aclRegistry = AclRegistry.fromFile(options.aclFile)
    } else {
      const aclFile = process.env['SODP_ACL_FILE']
      this.aclRegistry = aclFile ? AclRegistry.fromFile(aclFile) : null
    }

    if (options.authorize) {
      this.resolvedAuthorize = options.authorize
    } else if (this.aclRegistry) {
      const registry = this.aclRegistry
      this.resolvedAuthorize = (key, claims) => Promise.resolve(registry.canRead(key, claims))
    } else {
      this.resolvedAuthorize = () => Promise.resolve(true)
    }

    this.resolvedHydrate = options.hydrate ?? (() => Promise.resolve(null))

    const store = this.store
    const aclRegistry = this.aclRegistry
    const userOnCall = options.onCall

    this.resolvedOnCall = async (method, args, claims, session) => {
      const builtin = await handleBuiltinCall(method, args, claims, session, store, aclRegistry)
      if (builtin !== null) return builtin
      if (userOnCall) return userOnCall(method, args, claims, session)
      return { success: false, data: `unknown method: ${method}` }
    }
  }

  private hasAuthConfig(opts: SodpServerOptions): boolean {
    return !!(opts.jwtSecret || opts.jwtPublicKey || opts.jwtConfig || opts.authenticate ||
              jwtConfigFromEnv())
  }

  // Attach to an existing HTTP/HTTPS server. Multiple paths can be served by
  // calling attach() more than once with different SodpServer instances.
  attach(server: http.Server | https.Server, opts?: { path?: string }): void {
    this.wss = new WebSocketServer({ server, path: opts?.path ?? '/sodp' })
    this.wss.on('connection', (ws, req) => {
      const remote = req.socket.remoteAddress ?? 'unknown'
      this.handleConnection(ws, remote)
    })
  }

  // Start a standalone WebSocket server.
  listen(port: number, host = '0.0.0.0'): Promise<void> {
    this.wss = new WebSocketServer({ port, host })
    this.wss.on('connection', (ws, req) => {
      const remote = req.socket.remoteAddress ?? 'unknown'
      this.handleConnection(ws, remote)
    })
    return new Promise((resolve) => this.wss!.on('listening', resolve))
  }

  get address(): AddressInfo | null {
    if (!this.wss) return null
    return this.wss.address() as AddressInfo
  }

  close(): Promise<void> {
    return new Promise((resolve, reject) => {
      if (!this.wss) return resolve()
      this.wss.close((err) => (err ? reject(err) : resolve()))
    })
  }

  private handleConnection(ws: WebSocket, remote: string) {
    const session = new SodpSession(ws, {
      store: this.store,
      authenticate: this.resolvedAuthenticate,
      authorize: this.resolvedAuthorize,
      hydrate: this.resolvedHydrate,
      onCall: this.resolvedOnCall,
      authRequired: this.opts.authRequired,
      serverName: this.opts.serverName,
      rateLimitWrites: this.opts.rateLimitWrites,
      rateLimitWatches: this.opts.rateLimitWatches,
      backpressureLimit: this.opts.backpressureLimit,
    })
    void remote // available for logging if needed
    session.start()
  }

  // ── Server-side mutations ──────────────────────────────────────────────

  // Replace the full value of a key, broadcasting only the changed fields.
  set(key: string, value: unknown): void {
    const snap = this.store.snapshot(key)
    const ops = diffShallow(snap.value, value)
    if (ops.length > 0) {
      this.store.publish(key, ops)
    } else if (!snap.initialized) {
      this.store.hydrate(key, value)
    }
  }

  // Shallow-merge an object into an existing key.
  patch(key: string, patch: Record<string, unknown>): void {
    const ops = Object.entries(patch).map(([k, v]) => ({
      op: 'UPDATE' as const,
      path: `/${k.replace(/~/g, '~0').replace(/\//g, '~1')}`,
      value: v,
    }))
    if (ops.length > 0) this.store.publish(key, ops)
  }

  // Set a single nested field by JSON Pointer path.
  setIn(key: string, path: string, value: unknown): void {
    this.store.publish(key, [{ op: 'UPDATE', path, value }])
  }

  // Remove a key entirely, broadcasting to all watchers.
  delete(key: string): void {
    this.store.remove(key)
  }

  // Append an item to an array key. Optionally cap the array at maxLen items.
  append(key: string, item: unknown, maxLen?: number): void {
    const snap = this.store.snapshot(key)
    if (!snap.initialized || snap.value === null) {
      this.store.hydrate(key, [])
    }
    const ops: DeltaOp[] = [{ op: 'ADD', path: '/-', value: item }]
    if (maxLen !== undefined) {
      const arr = Array.isArray(snap.value) ? snap.value : []
      if (arr.length >= maxLen) {
        ops.unshift({ op: 'REMOVE', path: '/0' })
      }
    }
    this.store.publish(key, ops)
  }

  // Apply raw JSON Pointer ops to a key.
  mutate(key: string, ops: DeltaOp[]): void {
    if (ops.length > 0) this.store.publish(key, ops)
  }

  stats(): { keys: number; subscribers: number; connections: number } {
    const storeStats = this.store.stats()
    return {
      ...storeStats,
      connections: this.wss?.clients.size ?? 0,
    }
  }
}
