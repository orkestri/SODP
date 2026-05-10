// SODP TypeScript Server — Benchmark
//
// Measures fan-out latency and mutation throughput at different watcher counts.
// Run: npx ts-node bench/bench.ts [quick]
//
// Each scenario:
//   1. Start SodpServer on a random port.
//   2. Connect N clients, each watching "bench.key".
//   3. Warmup: fire ITERS_WARMUP mutations, discard timings.
//   4. Measure: fire ITERS mutations; record time from server.set() call to the
//      last client receiving its DELTA (end-to-end wall time).
//   5. Report P50, P95, P99, max, throughput.

import WebSocket from 'ws'
import { encode } from '@msgpack/msgpack'
import { SodpServer } from '../src/server'
import { decodeFrame, OP } from '../src/frame'
import type { Frame } from '../src/frame'

const QUICK = process.argv.includes('quick') || process.env['BENCH_QUICK'] === '1'
const ITERS_WARMUP = QUICK ? 50  : 200
const ITERS        = QUICK ? 200 : 1000
const WATCHER_COUNTS = QUICK ? [1, 10, 100] : [1, 10, 100, 1000]

// ── TestClient (minimal, no logging overhead) ─────────────────────────────────

interface WaiterEntry {
  resolve: (f: Frame) => void
  reject: (e: Error) => void
  timer: ReturnType<typeof setTimeout>
}

class BenchClient {
  ws: WebSocket
  private queue: Frame[] = []
  private waiters: WaiterEntry[] = []

  constructor(port: number) {
    this.ws = new WebSocket(`ws://127.0.0.1:${port}`)
    this.ws.binaryType = 'arraybuffer'
    this.ws.on('message', (data) => {
      const frame = decodeFrame(data as ArrayBuffer)
      const entry = this.waiters.shift()
      if (entry) {
        clearTimeout(entry.timer)
        entry.resolve(frame)
      } else {
        this.queue.push(frame)
      }
    })
  }

  connected(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.ws.once('open', resolve)
      this.ws.once('error', reject)
    })
  }

  recv(timeoutMs = 5000): Promise<Frame> {
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift()!)
    return new Promise((resolve, reject) => {
      const entry: WaiterEntry = {
        resolve,
        reject,
        timer: setTimeout(() => {
          const idx = this.waiters.indexOf(entry)
          if (idx !== -1) this.waiters.splice(idx, 1)
          reject(new Error('recv timeout'))
        }, timeoutMs),
      }
      this.waiters.push(entry)
    })
  }

  // Drain all queued frames without waiting.
  drain(): Frame[] {
    const out = this.queue.splice(0)
    return out
  }

  watch(key: string): void {
    this.ws.send(encode([OP.WATCH, 0, 2, { state: key }]))
  }

  close(): void { this.ws.close() }
}

// ── Stats ─────────────────────────────────────────────────────────────────────

function percentile(sorted: number[], p: number): number {
  const idx = Math.min(Math.ceil((p / 100) * sorted.length) - 1, sorted.length - 1)
  return sorted[Math.max(0, idx)]!
}

function fmtUs(ns: number): string {
  return `${(ns / 1000).toFixed(0)} µs`
}

function fmtMs(ns: number): string {
  return `${(ns / 1_000_000).toFixed(2)} ms`
}

// ── Scenario ──────────────────────────────────────────────────────────────────

async function runScenario(numWatchers: number): Promise<void> {
  const server = new SodpServer({ authRequired: false })
  await server.listen(0)
  const port = server.address!.port

  // Connect all watchers and subscribe.
  const clients: BenchClient[] = []
  for (let i = 0; i < numWatchers; i++) {
    const c = new BenchClient(port)
    await c.connected()
    clients.push(c)
  }

  // Drain HELLOs and STATE_INITs.
  await Promise.all(clients.map(async (c) => {
    c.watch('bench.key')
    await c.recv() // HELLO
    await c.recv() // STATE_INIT
  }))

  // ── Warmup ────────────────────────────────────────────────────────
  for (let i = 0; i < ITERS_WARMUP; i++) {
    server.set('bench.key', { v: i })
    await Promise.all(clients.map((c) => c.recv()))
  }

  // ── Measured iterations ───────────────────────────────────────────
  // Latency = hrtime from before server.set() to when ALL clients have received their DELTA.
  const latencies: number[] = []

  for (let i = 0; i < ITERS; i++) {
    const t0 = process.hrtime.bigint()
    server.set('bench.key', { v: i })
    await Promise.all(clients.map((c) => c.recv()))
    const t1 = process.hrtime.bigint()
    latencies.push(Number(t1 - t0))
  }

  latencies.sort((a, b) => a - b)

  const p50  = percentile(latencies, 50)
  const p95  = percentile(latencies, 95)
  const p99  = percentile(latencies, 99)
  const pMax = latencies[latencies.length - 1]!
  const totalNs = latencies.reduce((a, b) => a + b, 0)
  const throughput = (ITERS / (totalNs / 1e9)).toFixed(0)

  const label = String(numWatchers).padStart(4)
  if (numWatchers === 1) {
    // Single-client: report in µs
    console.log(
      `  ${label} watcher │ P50 ${fmtUs(p50).padStart(8)} │ P95 ${fmtUs(p95).padStart(8)} │ P99 ${fmtUs(p99).padStart(8)} │ max ${fmtUs(pMax).padStart(8)} │ ${throughput.padStart(7)} ops/s`
    )
  } else {
    // Multi-watcher: report in ms
    console.log(
      `  ${label} watchers │ P50 ${fmtMs(p50).padStart(9)} │ P95 ${fmtMs(p95).padStart(9)} │ P99 ${fmtMs(p99).padStart(9)} │ max ${fmtMs(pMax).padStart(9)} │ ${throughput.padStart(7)} ops/s`
    )
  }

  for (const c of clients) c.close()
  await server.close()
  // Let the OS reclaim the port.
  await new Promise((r) => setTimeout(r, 50))
}

// ── Throughput (single client, no waiting) ────────────────────────────────────

async function runThroughput(): Promise<void> {
  const server = new SodpServer({ authRequired: false })
  await server.listen(0)
  const port = server.address!.port

  const c = new BenchClient(port)
  await c.connected()
  c.watch('bench.key')
  await c.recv() // HELLO
  await c.recv() // STATE_INIT

  // Fire a burst and measure how fast the server can saturate the socket.
  const BURST = QUICK ? 500 : 2000
  const t0 = process.hrtime.bigint()
  for (let i = 0; i < BURST; i++) {
    server.set('bench.key', { v: i })
  }
  // Wait for all to arrive.
  for (let i = 0; i < BURST; i++) {
    await c.recv()
  }
  const t1 = process.hrtime.bigint()
  const elapsed = Number(t1 - t0)
  const ops = Math.round(BURST / (elapsed / 1e9))
  console.log(`  burst ${BURST}   │ total ${fmtMs(elapsed).padStart(9)} │ throughput ${ops.toLocaleString()} ops/s`)

  c.close()
  await server.close()
}

// ── Main ──────────────────────────────────────────────────────────────────────

async function main() {
  console.log(`\nSODP TypeScript Server Benchmark`)
  console.log(`Mode: ${QUICK ? 'quick' : 'full'} — ${ITERS} iters + ${ITERS_WARMUP} warmup\n`)

  console.log('── Fan-out latency (server.set → all watchers received DELTA) ──')
  for (const n of WATCHER_COUNTS) {
    await runScenario(n)
  }

  console.log('\n── Burst throughput (1 watcher, no per-op await) ──')
  await runThroughput()

  console.log()
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
