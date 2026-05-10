// SodpServer — the main entry point for embedding a SODP server in a Node.js app.
//
// Usage — attach to an existing HTTP server:
//   const server = new SodpServer({ jwtSecret: process.env.SODP_JWT_SECRET })
//   server.attach(httpServer, { path: '/sodp' })
//
// Usage — standalone with all features:
//   await server.listen(7777)
//
// Server-side mutations (fan-out to all watchers):
//   server.set('game.score', { value: 42 })
//   server.patch('game.player', { health: 80 })
//   server.setIn('game.player', '/position/x', 5)
//   server.delete('game.score')
//   server.append('game.events', event, 500)   // 500 = max array length
//   server.mutate('game.player', ops)

import { createServer as createHttpServer } from 'http'
import { WebSocketServer, type WebSocket } from 'ws'
import type http from 'http'
import type https from 'https'
import type { AddressInfo } from 'net'
import { StateStore } from './store'
import { SodpSession, type CallResult } from './session'
import { AclRegistry } from './acl'
import { SchemaRegistry } from './schema'
import { verifyJwt, jwtConfigFromEnv, type SodpClaims, type JwtConfig, type JwtPreset, type ClaimMappings } from './jwt'
import { handleBuiltinCall } from './rpc'
import { PersistenceManager } from './persist'
import { diffShallow } from './jsonpointer'
import type { DeltaOp } from './frame'

export type { SodpClaims, JwtConfig, JwtPreset, ClaimMappings } from './jwt'
export type { AclRule } from './acl'
export type { SchemaNode } from './schema'
export type { DeltaOp } from './frame'
export type { DeltaLogEntry } from './store'

export interface SodpServerOptions {
  // ── Auth ──────────────────────────────────────────────────────────────────
  // Pick one, or provide a custom authenticate hook.
  // If none is set, auth is disabled (authRequired defaults to false).
  jwtSecret?: string
  jwtPublicKey?: string       // RS256 public key PEM
  jwtConfig?: JwtConfig       // pre-built config, overrides the above
  jwtPreset?: JwtPreset       // idp preset: keycloak | auth0 | okta | cognito | generic
  claimMappings?: ClaimMappings

  // Custom auth hook — overrides JWT if provided.
  // Return SodpClaims on success, null to reject with 401.
  authenticate?: (token: string) => Promise<SodpClaims | null>

  // Whether AUTH frame is required. Defaults to true when any JWT config is set.
  authRequired?: boolean

  // ── ACL ───────────────────────────────────────────────────────────────────
  aclFile?: string
  acl?: AclRegistry
  // Custom ACL hook — overrides aclFile if provided. Return false → ERROR 403.
  authorize?: (key: string, claims: SodpClaims) => Promise<boolean>

  // ── Schema validation ──────────────────────────────────────────────────────
  schemaFile?: string
  schema?: SchemaRegistry

  // ── Hydration ─────────────────────────────────────────────────────────────
  // Called when a key is first watched and the store has no snapshot.
  // Return a value to seed initialized:true, or null/undefined for false.
  hydrate?: (key: string) => Promise<unknown | null>

  // ── CALL handler ──────────────────────────────────────────────────────────
  // Standard state.* methods are handled automatically before this is called.
  onCall?: (method: string, args: Record<string, unknown>, claims: SodpClaims, session: SodpSession) => Promise<CallResult>

  // ── Limits ────────────────────────────────────────────────────────────────
  backpressureLimit?: number
  rateLimitWrites?: number
  rateLimitWatches?: number
  // Maximum incoming WebSocket frame size in bytes. Default: 1 MiB.
  // Clients exceeding this are disconnected immediately.
  maxPayload?: number

  // ── Persistence ───────────────────────────────────────────────────────────
  // Directory for state snapshots. Written asynchronously (100ms debounce).
  // Loaded at startup so state survives process restarts.
  persistDir?: string

  // ── Redis clustering ──────────────────────────────────────────────────────
  // Redis URL for horizontal scaling. Requires: npm install ioredis
  // State is synced across nodes; cross-node RESUME falls back to STATE_INIT.
  redisUrl?: string

  // ── Observability ─────────────────────────────────────────────────────────
  // Port for GET /health → { status, connections, version }
  healthPort?: number
  // Port for GET /metrics → Prometheus text format
  metricsPort?: number

  // ── Misc ──────────────────────────────────────────────────────────────────
  serverName?: string
  store?: StateStore
}

export class SodpServer {
  readonly store: StateStore
  private wss: WebSocketServer | null = null
  private healthServer: http.Server | null = null
  private metricsServer: http.Server | null = null
  private persistence: PersistenceManager | null = null
  private cluster: import('./cluster').RedisCluster | null = null
  private shutdownHandlers: (() => void)[] = []

  private readonly opts: {
    authRequired: boolean
    backpressureLimit: number
    rateLimitWrites: number
    rateLimitWatches: number
    maxPayload: number
    serverName: string
  }
  private readonly resolvedAuthenticate: (token: string) => Promise<SodpClaims | null>
  private readonly resolvedAuthorize: (key: string, claims: SodpClaims) => Promise<boolean>
  private readonly resolvedHydrate: (key: string) => Promise<unknown | null>
  private readonly resolvedOnCall: (method: string, args: Record<string, unknown>, claims: SodpClaims, session: SodpSession) => Promise<CallResult>
  private readonly aclRegistry: AclRegistry | null
  private readonly schemaRegistry: SchemaRegistry | null

  constructor(options: SodpServerOptions = {}) {
    this.store = options.store ?? new StateStore()

    this.opts = {
      authRequired: options.authRequired ?? this.hasAuthConfig(options),
      backpressureLimit: options.backpressureLimit ?? parseInt(process.env['SODP_BACKPRESSURE_LIMIT'] ?? '1024', 10),
      rateLimitWrites:   options.rateLimitWrites   ?? parseInt(process.env['SODP_RATE_WRITES_PER_SEC'] ?? '50', 10),
      rateLimitWatches:  options.rateLimitWatches  ?? parseInt(process.env['SODP_RATE_WATCHES_PER_SEC'] ?? '10', 10),
      maxPayload:        options.maxPayload        ?? parseInt(process.env['SODP_MAX_PAYLOAD'] ?? String(1 * 1024 * 1024), 10),
      serverName: options.serverName ?? 'sodp-server-ts',
    }

    // ── JWT ──────────────────────────────────────────────────────────────────
    const jwtCfg: JwtConfig | null =
      options.jwtConfig ??
      (options.jwtSecret    ? { type: 'hs256', secret: options.jwtSecret, preset: options.jwtPreset, claimMappings: options.claimMappings } : null) ??
      (options.jwtPublicKey ? { type: 'rs256', publicKeyPem: options.jwtPublicKey, preset: options.jwtPreset, claimMappings: options.claimMappings } : null) ??
      jwtConfigFromEnv()

    if (options.authenticate) {
      this.resolvedAuthenticate = options.authenticate
    } else if (jwtCfg) {
      this.resolvedAuthenticate = (token) => verifyJwt(token, jwtCfg).catch(() => null)
    } else {
      this.resolvedAuthenticate = (token) =>
        Promise.resolve(token ? { sub: token, roles: [], groups: [], perms: [], extra: {} } : null)
    }

    // ── ACL ──────────────────────────────────────────────────────────────────
    this.aclRegistry =
      options.acl ?? (options.aclFile ? AclRegistry.fromFile(options.aclFile) : null) ??
      (process.env['SODP_ACL_FILE'] ? AclRegistry.fromFile(process.env['SODP_ACL_FILE']!) : null)

    if (options.authorize) {
      this.resolvedAuthorize = options.authorize
    } else if (this.aclRegistry) {
      const r = this.aclRegistry
      this.resolvedAuthorize = (key, claims) => Promise.resolve(r.canRead(key, claims))
    } else {
      this.resolvedAuthorize = () => Promise.resolve(true)
    }

    // ── Schema ───────────────────────────────────────────────────────────────
    this.schemaRegistry =
      options.schema ?? (options.schemaFile ? SchemaRegistry.fromFile(options.schemaFile) : null) ??
      (process.env['SODP_SCHEMA_FILE'] ? SchemaRegistry.fromFile(process.env['SODP_SCHEMA_FILE']!) : null)

    // ── Hydration + CALL ─────────────────────────────────────────────────────
    this.resolvedHydrate = options.hydrate ?? (() => Promise.resolve(null))

    const store = this.store
    const aclRegistry = this.aclRegistry
    const schemaRegistry = this.schemaRegistry
    const userOnCall = options.onCall

    this.resolvedOnCall = async (method, args, claims, session) => {
      const builtin = await handleBuiltinCall(method, args, claims, session, store, aclRegistry, schemaRegistry)
      if (builtin !== null) return builtin
      if (userOnCall) return userOnCall(method, args, claims, session)
      return { success: false, data: `unknown method: ${method}` }
    }

    // ── Persistence ───────────────────────────────────────────────────────────
    const persistDir = options.persistDir ?? process.env['SODP_PERSIST_DIR']
    if (persistDir) {
      this.persistence = new PersistenceManager(persistDir, this.store)
      this.persistence.load()
      this.persistence.start()
    }

    // ── Redis cluster (async init, kicked off lazily in listen/attach) ────────
    const redisUrl = options.redisUrl ?? process.env['SODP_REDIS_URL']
    if (redisUrl) {
      const { RedisCluster } = require('./cluster') as typeof import('./cluster')
      RedisCluster.connect(redisUrl, this.store).then((c) => {
        this.cluster = c
      }).catch((err: unknown) => {
        console.error('[sodp] Redis connection failed:', err)
      })
    }

    // ── Observability ─────────────────────────────────────────────────────────
    const healthPort = options.healthPort ?? parseInt(process.env['SODP_HEALTH_PORT'] ?? '0', 10)
    if (healthPort > 0) this.startHealthServer(healthPort)

    const metricsPort = options.metricsPort ?? parseInt(process.env['SODP_METRICS_PORT'] ?? '0', 10)
    if (metricsPort > 0) this.startMetricsServer(metricsPort)
  }

  private hasAuthConfig(opts: SodpServerOptions): boolean {
    return !!(opts.jwtSecret || opts.jwtPublicKey || opts.jwtConfig || opts.authenticate || jwtConfigFromEnv())
  }

  // ── Transport ─────────────────────────────────────────────────────────────

  attach(server: http.Server | https.Server, opts?: { path?: string }): void {
    this.wss = new WebSocketServer({ server, path: opts?.path ?? '/sodp', maxPayload: this.opts.maxPayload })
    this.wss.on('connection', (ws, req) => this.handleConnection(ws, req.socket.remoteAddress ?? 'unknown'))
  }

  async listen(port: number, host = '0.0.0.0'): Promise<void> {
    this.wss = new WebSocketServer({ port, host, maxPayload: this.opts.maxPayload })
    this.wss.on('connection', (ws, req) => this.handleConnection(ws, req.socket.remoteAddress ?? 'unknown'))
    await new Promise<void>((resolve) => this.wss!.on('listening', resolve))
  }

  // Register SIGTERM/SIGINT handlers for production use. Not called automatically
  // so that test suites don't accumulate process-level listeners.
  handleSignals(): void {
    this.registerShutdownHandlers()
  }

  get address(): AddressInfo | null {
    if (!this.wss) return null
    return this.wss.address() as AddressInfo
  }

  async close(): Promise<void> {
    this.persistence?.flushSync()
    this.persistence?.stop()
    await this.cluster?.close()
    // Terminate all open connections so wss.close() resolves immediately.
    for (const client of this.wss?.clients ?? []) client.terminate()
    await Promise.all([
      new Promise<void>((resolve, reject) => {
        if (!this.wss) return resolve()
        this.wss.close((err) => (err ? reject(err) : resolve()))
      }),
      new Promise<void>((resolve) => { if (!this.healthServer) return resolve(); this.healthServer.close(() => resolve()) }),
      new Promise<void>((resolve) => { if (!this.metricsServer) return resolve(); this.metricsServer.close(() => resolve()) }),
    ])
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
    void remote
    session.start()
  }

  // ── Graceful shutdown ─────────────────────────────────────────────────────

  private registerShutdownHandlers(): void {
    const handler = () => { void this.close().finally(() => process.exit(0)) }
    process.once('SIGTERM', handler)
    process.once('SIGINT',  handler)
    this.shutdownHandlers.push(handler)
  }

  // ── Observability ─────────────────────────────────────────────────────────

  private startHealthServer(port: number): void {
    this.healthServer = createHttpServer((req, res) => {
      if (req.url === '/health' && req.method === 'GET') {
        const { connections } = this.stats()
        res.writeHead(200, { 'Content-Type': 'application/json' })
        res.end(JSON.stringify({ status: 'ok', connections, version: '0.1.0' }))
      } else {
        res.writeHead(404)
        res.end()
      }
    })
    this.healthServer.listen(port)
  }

  private startMetricsServer(port: number): void {
    this.metricsServer = createHttpServer((req, res) => {
      if (req.url === '/metrics' && req.method === 'GET') {
        const { keys, subscribers, connections } = this.stats()
        const lines = [
          '# HELP sodp_connections_total Active WebSocket connections',
          '# TYPE sodp_connections_total gauge',
          `sodp_connections_total ${connections}`,
          '# HELP sodp_state_keys Total number of state keys in the store',
          '# TYPE sodp_state_keys gauge',
          `sodp_state_keys ${keys}`,
          '# HELP sodp_subscribers_total Total number of active key subscriptions',
          '# TYPE sodp_subscribers_total gauge',
          `sodp_subscribers_total ${subscribers}`,
        ]
        res.writeHead(200, { 'Content-Type': 'text/plain; version=0.0.4' })
        res.end(lines.join('\n') + '\n')
      } else {
        res.writeHead(404)
        res.end()
      }
    })
    this.metricsServer.listen(port)
  }

  // ── Server-side mutations ─────────────────────────────────────────────────

  set(key: string, value: unknown): void {
    const snap = this.store.snapshot(key)
    const ops = diffShallow(snap.value, value)
    if (ops.length > 0) {
      this.store.publish(key, ops)
    } else if (!snap.initialized) {
      this.store.hydrate(key, value)
    }
  }

  patch(key: string, patch: Record<string, unknown>): void {
    const ops = Object.entries(patch).map(([k, v]) => ({
      op: 'UPDATE' as const,
      path: `/${k.replace(/~/g, '~0').replace(/\//g, '~1')}`,
      value: v,
    }))
    if (ops.length > 0) this.store.publish(key, ops)
  }

  setIn(key: string, path: string, value: unknown): void {
    this.store.publish(key, [{ op: 'UPDATE', path, value }])
  }

  delete(key: string): void {
    this.store.remove(key)
  }

  append(key: string, item: unknown, maxLen?: number): void {
    const snap = this.store.snapshot(key)
    if (!snap.initialized || snap.value === null) this.store.hydrate(key, [])
    const ops: DeltaOp[] = [{ op: 'ADD', path: '/-', value: item }]
    if (maxLen !== undefined) {
      const arr = Array.isArray(snap.value) ? snap.value : []
      if (arr.length >= maxLen) ops.unshift({ op: 'REMOVE', path: '/0' })
    }
    this.store.publish(key, ops)
  }

  mutate(key: string, ops: DeltaOp[]): void {
    if (ops.length > 0) this.store.publish(key, ops)
  }

  stats(): { keys: number; subscribers: number; connections: number } {
    return { ...this.store.stats(), connections: this.wss?.clients.size ?? 0 }
  }
}
