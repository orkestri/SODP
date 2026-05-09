// bench is an intensive SODP benchmark that measures writer throughput and
// watcher fanout latency under configurable load.
//
// Two modes:
//
//  1. Single run  — fixed writers/watchers, live progress table, detailed summary.
//  2. Matrix mode — sweeps a watcher list and prints a scaling comparison table.
//
// Targets:
//
//	In-process Go server (default, labeled clearly).
//	Any external SODP server via -target (use cmd/sodp-server or the Rust server).
//
// Usage:
//
//	# In-process Go server, single run
//	go run ./cmd/bench
//
//	# External Rust server
//	go run ./cmd/bench -target ws://localhost:7777
//
//	# External Go server (start first: go run ./cmd/sodp-server -addr :7778)
//	go run ./cmd/bench -target ws://localhost:7778
//
//	# Scaling matrix (sweeps watcher counts, compares throughput/latency)
//	go run ./cmd/bench -matrix
//	go run ./cmd/bench -matrix -target ws://localhost:7777
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"math"
	"net/http"
	"net/http/httptest"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"

	sodp "github.com/orkestri/sodp-go"
)

// ── flags ────────────────────────────────────────────────────────────────────

var (
	flagTarget   = flag.String("target", "", "external SODP server URL (e.g. ws://localhost:7777); empty = in-process Go server")
	flagDuration = flag.Duration("duration", 20*time.Second, "benchmark duration (single-run mode)")
	flagWriters  = flag.Int("writers", 4, "concurrent writer connections")
	flagPipeline = flag.Int("pipeline", 16, "in-flight calls per writer (pipeline depth)")
	flagWatchers = flag.Int("watchers", 500, "concurrent watcher connections (single-run mode)")
	flagWarmup   = flag.Duration("warmup", 2*time.Second, "warmup duration before recording starts")
	flagKey      = flag.String("key", "bench", "state key")
	flagReport   = flag.Duration("report", 5*time.Second, "live progress interval (single-run mode)")

	flagMatrix         = flag.Bool("matrix", false, "sweep watcher counts and print a scaling table")
	flagWatcherList    = flag.String("watcher-list", "0,10,100,500,1000", "comma-separated watcher counts for matrix mode")
	flagMatrixDuration = flag.Duration("matrix-duration", 10*time.Second, "duration per matrix cell")
	flagMatrixWarmup   = flag.Duration("matrix-warmup", 2*time.Second, "warmup per matrix cell")

	// Multiplexing: pack multiple WATCH subscriptions onto fewer physical connections.
	// With -mux N, numWatchers/N connections are used, each subscribing to the same
	// key N times (N stream_ids). The server's write-pool drains all N pending DELTAs
	// in one goroutine pass, so OS-level TCP sends drop from numWatchers to numWatchers/N.
	flagMux = flag.Int("mux", 1, "WATCH subscriptions per watcher connection (1 = no mux)")
)

// ── connection ────────────────────────────────────────────────────────────────

type conn struct {
	ws *websocket.Conn
	mu sync.Mutex
}

func dialURL(url string) *conn {
	ws, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		log.Fatalf("dial %s: %v", url, err)
	}
	return &conn{ws: ws}
}

func (c *conn) send(f sodp.Frame) {
	data, err := sodp.EncodeFrame(f)
	if err != nil {
		log.Fatalf("encode: %v", err)
	}
	c.mu.Lock()
	c.ws.SetWriteDeadline(time.Now().Add(10 * time.Second))
	err = c.ws.WriteMessage(websocket.BinaryMessage, data)
	c.mu.Unlock()
	if err != nil {
		log.Fatalf("write: %v", err)
	}
}

func (c *conn) read() (sodp.Frame, int) {
	c.ws.SetReadDeadline(time.Now().Add(30 * time.Second))
	_, data, err := c.ws.ReadMessage()
	if err != nil {
		return sodp.Frame{}, 0
	}
	f, err := sodp.DecodeFrame(data)
	if err != nil {
		return sodp.Frame{}, len(data)
	}
	return f, len(data)
}

func (c *conn) close() { c.ws.Close() }

func readHello(c *conn) {
	f, _ := c.read()
	if f.Type != sodp.FrameHello {
		log.Fatalf("expected HELLO, got 0x%02x", f.Type)
	}
}

// ── stats ─────────────────────────────────────────────────────────────────────

type stats struct {
	mu        sync.Mutex
	latencies []time.Duration
}

func (s *stats) record(d time.Duration) {
	s.mu.Lock()
	s.latencies = append(s.latencies, d)
	s.mu.Unlock()
}

func (s *stats) snapshot() []time.Duration {
	s.mu.Lock()
	cp := make([]time.Duration, len(s.latencies))
	copy(cp, s.latencies)
	s.mu.Unlock()
	return cp
}

func pct(sorted []time.Duration, p float64) time.Duration {
	if len(sorted) == 0 {
		return 0
	}
	idx := int(math.Ceil(p/100*float64(len(sorted)))) - 1
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

func formatDur(d time.Duration) string {
	if d == 0 {
		return "—"
	}
	if d < time.Millisecond {
		return fmt.Sprintf("%dµs", d.Microseconds())
	}
	return fmt.Sprintf("%.1fms", float64(d.Microseconds())/1000)
}

func commaf(n int) string {
	s := fmt.Sprintf("%d", n)
	out := make([]byte, 0, len(s)+len(s)/3)
	for i, c := range s {
		if i > 0 && (len(s)-i)%3 == 0 {
			out = append(out, ',')
		}
		out = append(out, byte(c))
	}
	return string(out)
}

// ── payload ───────────────────────────────────────────────────────────────────

func makeValue(i int) map[string]any {
	return map[string]any{
		"counter": int64(i),
		"status":  "running",
		"node":    "bench",
		"score":   3.14159,
		"active":  true,
		"label":   "iteration",
		"meta":    map[string]any{"source": "bench", "rev": int64(i % 1000)},
		"tags":    []any{"sodp", "bench"},
	}
}

// ── writer ────────────────────────────────────────────────────────────────────

// runWriter sends pipelined CALLs using a semaphore. String call_ids ensure
// both Go and Rust servers echo them back correctly.
func runWriter(ctx context.Context, wsURL string, recording *atomic.Bool, st *stats, ops *atomic.Int64) {
	c := dialURL(wsURL)
	defer c.close()
	readHello(c)

	pipeline := *flagPipeline
	type pending struct{ sentAt time.Time }
	inflight := make(map[string]pending, pipeline)
	var mu sync.Mutex
	sem := make(chan struct{}, pipeline)

	var seq atomic.Int64

	// Reader drains RESULTs and releases semaphore slots.
	// The goroutine exits when c.read() returns a zero frame (connection closed).
	// defer c.close() (above) closes the connection when the main loop returns on
	// ctx.Done(), which unblocks this goroutine without needing a separate cancel goroutine.
	go func() {
		for {
			f, _ := c.read()
			if f.Type == 0 {
				return
			}
			if f.Type != sodp.FrameResult {
				continue
			}
			var callID string
			if m, ok := f.Body.(map[string]any); ok {
				callID, _ = m["call_id"].(string)
			}
			mu.Lock()
			p, ok := inflight[callID]
			if ok {
				delete(inflight, callID)
			}
			mu.Unlock()
			if ok {
				<-sem
				if recording.Load() {
					st.record(time.Since(p.sentAt))
					ops.Add(1)
				}
			}
		}
	}()

	for {
		select {
		case <-ctx.Done():
			return
		case sem <- struct{}{}:
		}
		id := seq.Add(1)
		callID := strconv.FormatInt(id, 10)
		sentAt := time.Now()
		mu.Lock()
		inflight[callID] = pending{sentAt: sentAt}
		mu.Unlock()
		c.send(sodp.Frame{
			Type:     sodp.FrameCall,
			StreamID: 1,
			Seq:      uint64(id),
			Body: map[string]any{
				"call_id": callID,
				"method":  "state.set",
				"args":    map[string]any{"state": *flagKey, "value": makeValue(int(id))},
			},
		})
	}
}

// ── watcher ───────────────────────────────────────────────────────────────────

// watchGroup manages a pool of watcher connections.
type watchGroup struct {
	conns      []*conn
	totalBytes atomic.Int64
	deltaCount atomic.Int64
}

func (wg *watchGroup) closeAll() {
	for _, c := range wg.conns {
		c.ws.Close()
	}
}

// startWatchers connects numWatchers logical subscriptions using ceiling(numWatchers/mux)
// physical WebSocket connections. Each connection sends mux WATCH frames for the same
// key (with distinct stream_ids) so the server writes mux DELTAs per mutation to one
// TCP connection — the pool drains them all in one batch.
func startWatchers(wsURL string, numWatchers, mux int) *watchGroup {
	numConns := (numWatchers + mux - 1) / mux
	wg := &watchGroup{conns: make([]*conn, numConns)}
	var mu sync.Mutex
	var eg sync.WaitGroup
	for i := 0; i < numConns; i++ {
		eg.Add(1)
		go func(connID int) {
			defer eg.Done()
			c := dialURL(wsURL)
			mu.Lock()
			wg.conns[connID] = c
			mu.Unlock()
			readHello(c)
			// Subscribe mux times to the same key, each with a unique stream_id.
			// The server fanout sends mux DELTAs to this session per mutation.
			for sub := 0; sub < mux; sub++ {
				streamID := uint32(1000 + connID*mux + sub)
				c.send(sodp.Frame{
					Type:     sodp.FrameWatch,
					StreamID: streamID,
					Body:     map[string]any{"state": *flagKey},
				})
				c.read() // STATE_INIT
			}
		}(i)
	}
	eg.Wait()
	return wg
}

func runWatcher(ctx context.Context, c *conn, wg *watchGroup) {
	defer c.close()
	for {
		_, data, err := c.ws.ReadMessage()
		if err != nil {
			return
		}
		f, err := sodp.DecodeFrame(data)
		if err != nil || f.Type != sodp.FrameDelta {
			continue
		}
		wg.totalBytes.Add(int64(len(data)))
		wg.deltaCount.Add(1)
	}
}

// ── bench run ─────────────────────────────────────────────────────────────────

type result struct {
	duration     time.Duration
	mutations    int64
	latencies    []time.Duration // sorted
	watcherBytes int64
	deltaCount   int64
	heapMB       float64
	goroutines   int
}

func runBench(wsURL string, numWatchers, mux int, warmup, duration time.Duration) result {
	// Connect watchers first.
	var wg *watchGroup
	if numWatchers > 0 {
		wg = startWatchers(wsURL, numWatchers, mux)
		time.Sleep(100 * time.Millisecond) // let subscriptions propagate
	}

	// Start writers (warmup phase — not recording).
	writeCtx, writeCancel := context.WithCancel(context.Background())
	var recording atomic.Bool
	var ops atomic.Int64
	st := &stats{}

	var wwg sync.WaitGroup
	for i := 0; i < *flagWriters; i++ {
		wwg.Add(1)
		go func() {
			defer wwg.Done()
			runWriter(writeCtx, wsURL, &recording, st, &ops)
		}()
	}

	// Watcher read loops — started BEFORE warmup so they're draining from the start.
	var rwg sync.WaitGroup
	watchCtx, watchCancel := context.WithCancel(context.Background())
	if wg != nil {
		for _, c := range wg.conns {
			c := c
			rwg.Add(1)
			go func() {
				defer rwg.Done()
				runWatcher(watchCtx, c, wg)
			}()
		}
	}

	time.Sleep(warmup)

	// Measured run.
	recording.Store(true)
	if wg != nil {
		wg.totalBytes.Store(0)
		wg.deltaCount.Store(0)
	}
	start := time.Now()
	time.Sleep(duration)
	elapsed := time.Since(start)

	recording.Store(false)
	writeCancel()
	wwg.Wait()

	watchCancel()
	if wg != nil {
		wg.closeAll() // unblocks runWatcher goroutines
	}
	rwg.Wait()

	// Collect results.
	lats := st.snapshot()
	sort.Slice(lats, func(i, j int) bool { return lats[i] < lats[j] })

	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)

	var wb, dc int64
	if wg != nil {
		wb = wg.totalBytes.Load()
		dc = wg.deltaCount.Load()
	}

	return result{
		duration:     elapsed,
		mutations:    ops.Load(),
		latencies:    lats,
		watcherBytes: wb,
		deltaCount:   dc,
		heapMB:       float64(mem.HeapInuse) / (1024 * 1024),
		goroutines:   runtime.NumGoroutine(),
	}
}

// ── single run ────────────────────────────────────────────────────────────────

func printSummary(label string, r result, numWatchers int) {
	throughput := float64(r.mutations) / r.duration.Seconds()
	fmt.Printf("\n%s\n", label)
	fmt.Printf("  duration:    %v\n", r.duration.Round(time.Millisecond))
	fmt.Printf("  mutations:   %s\n", commaf(int(r.mutations)))
	fmt.Printf("  throughput:  %s ops/sec\n", commaf(int(throughput)))
	fmt.Printf("  P50:         %s\n", formatDur(pct(r.latencies, 50)))
	fmt.Printf("  P95:         %s\n", formatDur(pct(r.latencies, 95)))
	fmt.Printf("  P99:         %s\n", formatDur(pct(r.latencies, 99)))
	fmt.Printf("  P999:        %s\n", formatDur(pct(r.latencies, 99.9)))
	if len(r.latencies) > 0 {
		fmt.Printf("  max:         %s\n", formatDur(r.latencies[len(r.latencies)-1]))
	}
	if r.watcherBytes > 0 && r.mutations > 0 {
		bpw := r.watcherBytes / r.mutations / int64(numWatchers)
		fmt.Printf("  bytes/watcher/round: %d B\n", bpw)
	}
	if r.deltaCount > 0 && numWatchers > 0 {
		expected := r.mutations * int64(numWatchers)
		pct := float64(r.deltaCount) / float64(expected) * 100
		fmt.Printf("  delta delivery: %s / %s (%.1f%%)\n",
			commaf(int(r.deltaCount)), commaf(int(expected)), pct)
	}
	fmt.Printf("  heap:        %.1f MB\n", r.heapMB)
	fmt.Printf("  goroutines:  %d\n", r.goroutines)
}

func singleRun(wsURL string) {
	numWatchers := *flagWatchers
	mux := *flagMux
	if mux < 1 {
		mux = 1
	}
	numConns := (numWatchers + mux - 1) / mux
	fmt.Printf("writers:     %d  (pipeline: %d  →  %d max in-flight)\n",
		*flagWriters, *flagPipeline, *flagWriters**flagPipeline)
	if mux == 1 {
		fmt.Printf("watchers:    %d  (1 subscription per connection)\n", numWatchers)
	} else {
		fmt.Printf("watchers:    %d  (mux %d → %d connections × %d subs)\n",
			numWatchers, mux, numConns, mux)
	}
	fmt.Printf("duration:    %v  (warmup: %v)\n", *flagDuration, *flagWarmup)
	fmt.Printf("key:         %s\n\n", *flagKey)

	// Connect watchers up front so we can show progress while they connect.
	var wg *watchGroup
	if numWatchers > 0 {
		fmt.Printf("connecting %d watchers (%d connections)... ", numWatchers, numConns)
		wg = startWatchers(wsURL, numWatchers, mux)
		time.Sleep(100 * time.Millisecond)
		fmt.Printf("ok\n")
	}

	fmt.Printf("warming up for %v... ", *flagWarmup)

	writeCtx, writeCancel := context.WithCancel(context.Background())
	var recording atomic.Bool
	var ops atomic.Int64
	st := &stats{}

	var writers sync.WaitGroup
	for i := 0; i < *flagWriters; i++ {
		writers.Add(1)
		go func() {
			defer writers.Done()
			runWriter(writeCtx, wsURL, &recording, st, &ops)
		}()
	}

	watchCtx, watchCancel := context.WithCancel(context.Background())
	var rwg sync.WaitGroup
	if wg != nil {
		for _, c := range wg.conns {
			c := c
			rwg.Add(1)
			go func() {
				defer rwg.Done()
				runWatcher(watchCtx, c, wg)
			}()
		}
	}

	time.Sleep(*flagWarmup)
	fmt.Printf("ok\n\n")

	recording.Store(true)
	if wg != nil {
		wg.totalBytes.Store(0)
		wg.deltaCount.Store(0)
	}

	start := time.Now()
	ticker := time.NewTicker(*flagReport)
	deadline := time.NewTimer(*flagDuration)

	var lastOps int64
	var lastTime = start

	fmt.Printf("%-8s  %-18s  %-10s  %-10s  %-10s  %-10s  %-10s  %-10s\n",
		"elapsed", "throughput", "P50", "P95", "P99", "P999", "goroutines", "heap")
	fmt.Printf("%s\n", strings.Repeat("─", 100))

	running := true
	for running {
		select {
		case t := <-ticker.C:
			snap := st.snapshot()
			sort.Slice(snap, func(i, j int) bool { return snap[i] < snap[j] })
			curOps := ops.Load()
			interval := t.Sub(lastTime).Seconds()
			tput := float64(curOps-lastOps) / interval
			lastOps = curOps
			lastTime = t
			var mem runtime.MemStats
			runtime.ReadMemStats(&mem)
			fmt.Printf("%-8s  %-18s  %-10s  %-10s  %-10s  %-10s  %-10d  %.1f MB\n",
				time.Since(start).Round(time.Second),
				commaf(int(tput))+" ops/sec",
				formatDur(pct(snap, 50)),
				formatDur(pct(snap, 95)),
				formatDur(pct(snap, 99)),
				formatDur(pct(snap, 99.9)),
				runtime.NumGoroutine(),
				float64(mem.HeapInuse)/(1024*1024),
			)
		case <-deadline.C:
			running = false
		}
	}

	elapsed := time.Since(start)
	recording.Store(false)
	writeCancel()
	writers.Wait()

	watchCancel()
	if wg != nil {
		wg.closeAll()
	}
	rwg.Wait()

	lats := st.snapshot()
	sort.Slice(lats, func(i, j int) bool { return lats[i] < lats[j] })
	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)
	var wb, dc int64
	if wg != nil {
		wb = wg.totalBytes.Load()
		dc = wg.deltaCount.Load()
	}

	printSummary("Final results", result{
		duration:     elapsed,
		mutations:    ops.Load(),
		latencies:    lats,
		watcherBytes: wb,
		deltaCount:   dc,
		heapMB:       float64(mem.HeapInuse) / (1024 * 1024),
		goroutines:   runtime.NumGoroutine(),
	}, numWatchers)
}

// ── matrix mode ───────────────────────────────────────────────────────────────

func parseWatcherList(s string) []int {
	parts := strings.Split(s, ",")
	counts := make([]int, 0, len(parts))
	for _, p := range parts {
		p = strings.TrimSpace(p)
		n, err := strconv.Atoi(p)
		if err != nil || n < 0 {
			log.Fatalf("invalid watcher count %q in -watcher-list", p)
		}
		counts = append(counts, n)
	}
	return counts
}

func matrixRun(wsURL string) {
	counts := parseWatcherList(*flagWatcherList)
	mux := *flagMux
	if mux < 1 {
		mux = 1
	}

	fmt.Printf("writers: %d  pipeline: %d  →  %d max in-flight\n",
		*flagWriters, *flagPipeline, *flagWriters**flagPipeline)
	fmt.Printf("duration per cell: %v  warmup: %v\n", *flagMatrixDuration, *flagMatrixWarmup)
	if mux > 1 {
		fmt.Printf("mux: %d subscriptions per connection\n", mux)
	}
	fmt.Printf("key: %s\n\n", *flagKey)
	fmt.Printf("%-10s  %-8s  %-14s  %-10s  %-10s  %-10s  %-14s  %-10s\n",
		"watchers", "conns", "ops/sec", "P50", "P95", "P99", "bytes/watcher", "heap")
	fmt.Printf("%s\n", strings.Repeat("─", 98))

	for _, n := range counts {
		r := runBench(wsURL, n, mux, *flagMatrixWarmup, *flagMatrixDuration)
		throughput := float64(r.mutations) / r.duration.Seconds()
		numConns := (n + mux - 1) / mux
		if n == 0 {
			numConns = 0
		}

		bpw := "—"
		if n > 0 && r.mutations > 0 {
			bpw = fmt.Sprintf("%d B", r.watcherBytes/r.mutations/int64(n))
		}

		fmt.Printf("%-10d  %-8d  %-14s  %-10s  %-10s  %-10s  %-14s  %.1f MB\n",
			n,
			numConns,
			commaf(int(throughput))+" ops/s",
			formatDur(pct(r.latencies, 50)),
			formatDur(pct(r.latencies, 95)),
			formatDur(pct(r.latencies, 99)),
			bpw,
			r.heapMB,
		)

		// GC between cells to get a clean heap reading for the next one.
		runtime.GC()
	}
	fmt.Println()
}

// ── main ──────────────────────────────────────────────────────────────────────

func main() {
	flag.Parse()

	wsURL := *flagTarget
	var ts *httptest.Server

	if wsURL == "" {
		srv := sodp.NewServer(
			sodp.WithBackpressureLimit(256),
			sodp.WithRateLimit(10_000_000),
			sodp.WithMaxSessions(10_000),
			sodp.WithMaxWatches(1024),
		)
		mux := http.NewServeMux()
		mux.HandleFunc("/sodp", srv.HandleWS)
		ts = httptest.NewServer(mux)
		wsURL = "ws" + strings.TrimPrefix(ts.URL, "http") + "/sodp"
		defer ts.Close()
		fmt.Printf("target:  in-process Go server (%s)\n", ts.URL)
		fmt.Printf("         *** NOTE: server goroutines share CPU with the bench process.\n")
		fmt.Printf("         *** For a fair comparison, run ./cmd/sodp-server and use -target.\n\n")
	} else {
		fmt.Printf("target:  %s\n\n", wsURL)
	}

	if *flagMatrix {
		matrixRun(wsURL)
	} else {
		singleRun(wsURL)
	}
}
