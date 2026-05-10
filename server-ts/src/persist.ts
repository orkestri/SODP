// File-based state persistence for single-node deployments.
//
// On every mutation the store is snapshotted asynchronously to
// {persistDir}/sodp-state.json. Writes are debounced (100ms window) so a
// burst of mutations produces one write, not N. On startup the snapshot is
// loaded back into the StateStore.
//
// Not crash-safe to the last 100ms (no WAL), but adequate for most embedded
// use cases. For durability requirements, use the Rust server.

import { readFileSync, writeFileSync, mkdirSync, existsSync, writeFile } from 'fs'
import { join } from 'path'
import type { StateStore } from './store'

interface SnapshotEntry {
  version: number
  value: unknown
}

type Snapshot = Record<string, SnapshotEntry>

export class PersistenceManager {
  private readonly filePath: string
  private readonly store: StateStore
  private flushTimer: ReturnType<typeof setTimeout> | null = null
  private unsubGlobal: (() => void) | null = null
  private closed = false

  constructor(persistDir: string, store: StateStore) {
    this.store = store
    mkdirSync(persistDir, { recursive: true })
    this.filePath = join(persistDir, 'sodp-state.json')
  }

  // Load snapshot from disk and hydrate the store.
  load(): void {
    if (!existsSync(this.filePath)) return
    try {
      const raw = readFileSync(this.filePath, 'utf-8')
      const snapshot = JSON.parse(raw) as Snapshot
      const entries = Object.entries(snapshot).map(([key, { version, value }]) => ({
        key, version, value,
      }))
      this.store.loadEntries(entries)
    } catch { /* corrupt file — start fresh */ }
  }

  // Start listening to store mutations and writing snapshots.
  start(): void {
    this.unsubGlobal = this.store.addGlobalSubscriber(() => {
      this.scheduleSave()
    })
  }

  private scheduleSave(): void {
    if (this.closed) return
    if (this.flushTimer) return // already scheduled
    this.flushTimer = setTimeout(() => {
      this.flushTimer = null
      this.saveNow().catch((err) => { console.error('[sodp] persistence write failed:', err) })
    }, 100)
  }

  private async saveNow(): Promise<void> {
    if (this.closed) return
    const snapshot: Snapshot = {}
    const stats = this.store.stats()
    // We need all keys — use the store's snapshot method for each.
    // Access via an internal snapshot helper on the store.
    for (const key of this.store.allKeys()) {
      const snap = this.store.snapshot(key)
      if (snap.initialized) {
        snapshot[key] = { version: snap.version, value: snap.value }
      }
    }
    await new Promise<void>((resolve, reject) =>
      writeFile(this.filePath, JSON.stringify(snapshot), 'utf-8', (err) => err ? reject(err) : resolve())
    )
  }

  // Flush synchronously before process exit.
  flushSync(): void {
    this.closed = true
    if (this.flushTimer) { clearTimeout(this.flushTimer); this.flushTimer = null }
    const snapshot: Snapshot = {}
    for (const key of this.store.allKeys()) {
      const snap = this.store.snapshot(key)
      if (snap.initialized) {
        snapshot[key] = { version: snap.version, value: snap.value }
      }
    }
    try { writeFileSync(this.filePath, JSON.stringify(snapshot), 'utf-8') } catch (err) { console.error('[sodp] persistence flush failed:', err) }
  }

  stop(): void {
    this.closed = true
    if (this.flushTimer) { clearTimeout(this.flushTimer); this.flushTimer = null }
    this.unsubGlobal?.()
  }
}
