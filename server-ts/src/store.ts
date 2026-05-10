// In-memory key/value store with per-key delta log.
//
// Each key carries:
//   - the current value + its version number
//   - a ring buffer of the last N deltas for RESUME replay
//   - the set of subscriber callbacks for fan-out
//
// If a client's since_version is older than the buffered window we send
// STATE_INIT instead of replaying.

import type { DeltaOp } from './frame'
import { applyOps } from './jsonpointer'

const DEFAULT_LOG_CAPACITY = 1024

export interface DeltaLogEntry {
  version: number
  ops: DeltaOp[]
  at: number
}

export interface Subscriber {
  (delta: DeltaLogEntry, key: string): void
}

interface Entry {
  value: unknown
  version: number
  initialized: boolean
  log: DeltaLogEntry[]
  subscribers: Set<Subscriber>
}

export class StateStore {
  private readonly keys = new Map<string, Entry>()
  private readonly capacity: number

  constructor(capacity = DEFAULT_LOG_CAPACITY) {
    this.capacity = capacity
  }

  private getOrInit(key: string): Entry {
    let e = this.keys.get(key)
    if (!e) {
      e = { value: null, version: 0, initialized: false, log: [], subscribers: new Set() }
      this.keys.set(key, e)
    }
    return e
  }

  snapshot(key: string): { version: number; value: unknown; initialized: boolean } {
    const e = this.keys.get(key)
    if (!e) return { version: 0, value: null, initialized: false }
    return { version: e.version, value: e.value, initialized: e.initialized }
  }

  // Returns deltas strictly newer than sinceVersion, or null if the log window
  // doesn't cover that far back (caller must fall back to STATE_INIT).
  deltasSince(key: string, sinceVersion: number): DeltaLogEntry[] | null {
    const e = this.keys.get(key)
    if (!e) return null
    const oldest = e.log[0]?.version
    if (oldest === undefined || sinceVersion < oldest - 1) return null
    return e.log.filter((d) => d.version > sinceVersion)
  }

  publish(key: string, ops: DeltaOp[]): DeltaLogEntry {
    const e = this.getOrInit(key)
    e.value = applyOps(e.value, ops)
    e.version += 1
    e.initialized = true
    const entry: DeltaLogEntry = { version: e.version, ops, at: Date.now() }
    e.log.push(entry)
    if (e.log.length > this.capacity) e.log.shift()
    for (const sub of e.subscribers) {
      try { sub(entry, key) } catch { /* subscriber bugs must not kill others */ }
    }
    return entry
  }

  // Seed a key with a full value without broadcasting. Use when loading
  // an existing value from a database before the first WATCH arrives.
  hydrate(key: string, value: unknown): void {
    const e = this.getOrInit(key)
    e.value = value
    e.initialized = true
    // Version is not bumped — hydration is a silent snapshot replacement.
  }

  // Remove a key entirely and broadcast a root REMOVE op.
  remove(key: string): void {
    const e = this.keys.get(key)
    if (!e) return
    this.publish(key, [{ op: 'REMOVE', path: '/' }])
    // Clear the entry after broadcasting so late subscribers see null.
    e.value = null
    e.version = 0
    e.initialized = false
    e.log = []
  }

  subscribe(key: string, sub: Subscriber): () => void {
    const e = this.getOrInit(key)
    e.subscribers.add(sub)
    return () => e.subscribers.delete(sub)
  }

  stats(): { keys: number; subscribers: number } {
    let subs = 0
    for (const e of this.keys.values()) subs += e.subscribers.size
    return { keys: this.keys.size, subscribers: subs }
  }
}
