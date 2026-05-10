// Redis-backed horizontal scaling for SODP TypeScript server.
//
// When SODP_REDIS_URL is set (or redisUrl option provided):
//   1. On startup — loads all state from Redis into the local StateStore.
//   2. On every local mutation — fire-and-forget: HSET state + PUBLISH delta.
//   3. Background subscriber — receives deltas from other nodes, delivers to
//      local watchers via store.publishRemote() (skips re-broadcast to Redis).
//
// Cross-node RESUME falls back to STATE_INIT (same graceful path as log eviction).
// This module uses ioredis; install it separately: npm install ioredis

import { randomUUID } from 'crypto'
import type { StateStore } from './store'
import type { DeltaOp } from './frame'

// Lazy-load ioredis so projects that don't use Redis don't need to install it.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function loadIoRedis(): { new(url: string): RedisLike } {
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const mod = require('ioredis') as { default?: unknown }
    // ioredis exports the class as default or as the module itself.
    const Redis = mod.default ?? mod
    return Redis as { new(url: string): RedisLike }
  } catch {
    throw new Error(
      '@sodp/server: Redis clustering requires ioredis — run: npm install ioredis'
    )
  }
}

interface RedisLike {
  hgetall(key: string): Promise<Record<string, string> | null>
  hset(key: string, field: string, value: string): Promise<unknown>
  publish(channel: string, message: string): Promise<unknown>
  psubscribe(pattern: string): Promise<unknown>
  on(event: string, handler: (...args: unknown[]) => void): void
  quit(): Promise<unknown>
  duplicate(): RedisLike
}

interface CrossNodeDelta {
  nodeId: string
  key: string
  version: number
  ops: DeltaOp[]
}

export class RedisCluster {
  private readonly nodeId = randomUUID()
  private readonly pub: RedisLike
  private readonly sub: RedisLike
  private readonly store: StateStore
  private unsubGlobal: (() => void) | null = null

  private constructor(pub: RedisLike, sub: RedisLike, store: StateStore) {
    this.pub = pub
    this.sub = sub
    this.store = store
  }

  static async connect(url: string, store: StateStore): Promise<RedisCluster> {
    const Redis = loadIoRedis()
    const pub = new Redis(url)
    const sub = pub.duplicate()
    const cluster = new RedisCluster(pub, sub, store)
    await cluster.bootstrap()
    cluster.startSubscriber()
    cluster.hookStore()
    return cluster
  }

  // Load all persisted state from Redis so this node starts current.
  private async bootstrap(): Promise<void> {
    const raw = await this.pub.hgetall('sodp:state')
    if (!raw) return
    const entries: Array<{ key: string; version: number; value: unknown }> = []
    for (const [key, json] of Object.entries(raw)) {
      try {
        const { version, value } = JSON.parse(json) as { version: number; value: unknown }
        entries.push({ key, version, value })
      } catch { /* corrupt entry — skip */ }
    }
    if (entries.length > 0) this.store.loadEntries(entries)
  }

  // Subscribe to delta events from peer nodes.
  private startSubscriber(): void {
    this.sub.psubscribe('sodp:delta:*').catch(() => { /* ignore startup errors */ })
    this.sub.on('pmessage', (_pattern: unknown, _channel: unknown, message: unknown) => {
      try {
        const delta = JSON.parse(message as string) as CrossNodeDelta
        if (delta.nodeId === this.nodeId) return // skip own messages
        this.store.publishRemote(delta.key, delta.ops, delta.version)
      } catch { /* malformed message — ignore */ }
    })
  }

  // Hook into the store to sync every local mutation to Redis.
  private hookStore(): void {
    this.unsubGlobal = this.store.addGlobalSubscriber((key, entry, value) => {
      // Fire-and-forget — zero latency impact on the mutation hot path.
      const stateJson = JSON.stringify({ version: entry.version, value })
      const deltaJson = JSON.stringify({
        nodeId: this.nodeId,
        key,
        version: entry.version,
        ops: entry.ops,
      } satisfies CrossNodeDelta)

      Promise.all([
        this.pub.hset('sodp:state', key, stateJson),
        this.pub.publish(`sodp:delta:${key}`, deltaJson),
      ]).catch(() => { /* Redis errors are non-fatal */ })
    })
  }

  async close(): Promise<void> {
    this.unsubGlobal?.()
    await Promise.all([this.pub.quit(), this.sub.quit()])
  }
}
