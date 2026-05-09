// bench is an intensive SODP benchmark with pipelined writers, high watcher
// counts, duration-based measurement, and live progress reporting.
//
// It can run against either an in-process Go server (default) or any external
// SODP server via -target, enabling direct comparison between implementations.
//
// Usage:
//
//	# Against in-process Go server
//	go run ./cmd/bench
//
//	# Against external Rust reference server (start first: cargo run --bin sodp-server -- 0.0.0.0:7777)
//	go run ./cmd/bench -target ws://localhost:7777
//
//	# Full stress test
//	go run ./cmd/bench -writers=8 -pipeline=32 -watchers=1000 -duration=30s
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
	flagDuration = flag.Duration("duration", 20*time.Second, "benchmark duration")
	flagWriters  = flag.Int("writers", 4, "concurrent writer connections")
	flagPipeline = flag.Int("pipeline", 16, "in-flight calls per writer (pipeline depth)")
	flagWatchers = flag.Int("watchers", 500, "concurrent watcher connections")
	flagWarmup   = flag.Duration("warmup", 2*time.Second, "warmup duration before recording starts")
	flagKey      = flag.String("key", "bench", "state key")
	flagReport   = flag.Duration("report", 5*time.Second, "live progress interval")
)

// ── connection ────────────────────────────────────────────────────────────────

type conn struct {
	ws  *websocket.Conn
	mu  sync.Mutex // serialize writes (multiple goroutines may call send)
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

// readHello drains HELLO frame, returns whether auth is required.
func readHello(c *conn) bool {
	f, _ := c.read()
	if f.Type != sodp.FrameHello {
		log.Fatalf("expected HELLO, got 0x%02x", f.Type)
	}
	if m, ok := f.Body.(map[string]any); ok {
		if auth, ok := m["auth"].(bool); ok {
			return auth
		}
	}
	return false
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
	if d < time.Millisecond {
		return fmt.Sprintf("%dµs", d.Microseconds())
	}
	return fmt.Sprintf("%.2fms", float64(d.Microseconds())/1000)
}

func printSummary(label string, lats []time.Duration, elapsed time.Duration, mutations int64, watcherBytes int64) {
	s := make([]time.Duration, len(lats))
	copy(s, lats)
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })

	throughput := float64(mutations) / elapsed.Seconds()
	var maxLat time.Duration
	if len(s) > 0 {
		maxLat = s[len(s)-1]
	}

	fmt.Printf("\n%s\n", label)
	fmt.Printf("  duration:    %v\n", elapsed.Round(time.Millisecond))
	fmt.Printf("  mutations:   %s\n", commaf(int(mutations)))
	fmt.Printf("  throughput:  %s ops/sec\n", commaf(int(throughput)))
	fmt.Printf("  P50:         %s\n", formatDur(pct(s, 50)))
	fmt.Printf("  P95:         %s\n", formatDur(pct(s, 95)))
	fmt.Printf("  P99:         %s\n", formatDur(pct(s, 99)))
	fmt.Printf("  P999:        %s\n", formatDur(pct(s, 99.9)))
	fmt.Printf("  max:         %s\n", formatDur(maxLat))
	if watcherBytes > 0 && mutations > 0 {
		fmt.Printf("  bytes/round: %s (%d watchers × ~%d bytes)\n",
			commaf(int(watcherBytes/mutations)),
			*flagWatchers,
			watcherBytes/mutations/int64(*flagWatchers))
	}
}

func commaf(n int) string {
	s := fmt.Sprintf("%d", n)
	out := []byte{}
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
		"counter":  int64(i),
		"status":   "running",
		"node":     "bench",
		"score":    3.14159,
		"active":   true,
		"label":    "iteration",
		"meta":     map[string]any{"source": "bench", "rev": int64(i % 1000)},
		"tags":     []any{"sodp", "bench"},
	}
}

// ── writer ────────────────────────────────────────────────────────────────────
//
// Each writer uses a semaphore of size `pipeline` to keep that many CALLs
// in flight concurrently. A separate reader goroutine drains RESULTs and
// records latency.

func runWriter(ctx context.Context, wsURL string, writerID int, recording *atomic.Bool, st *stats, ops *atomic.Int64) {
	c := dialURL(wsURL)
	defer c.close()
	readHello(c)

	pipeline := *flagPipeline

	// call_id → sent-at timestamp; use string keys so both Go and Rust echo them correctly.
	type pending struct {
		sentAt time.Time
	}
	inflight := make(map[string]pending, pipeline)
	var mu sync.Mutex
	sem := make(chan struct{}, pipeline)

	var seq atomic.Int64

	// Close connection on context cancellation to unblock the reader goroutine.
	go func() { <-ctx.Done(); c.ws.Close() }()

	// Reader drains RESULTs.
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
				<-sem // release pipeline slot
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
		case sem <- struct{}{}: // acquire pipeline slot
		}

		id := seq.Add(1)
		callID := fmt.Sprintf("%d", id)
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

func runWatcher(ctx context.Context, wsURL string, watcherID int, totalBytes *atomic.Int64, deltaCount *atomic.Int64) {
	c := dialURL(wsURL)
	defer c.close()

	// Close connection on context cancellation to unblock ReadMessage.
	go func() { <-ctx.Done(); c.ws.Close() }()

	readHello(c)

	c.send(sodp.Frame{
		Type:     sodp.FrameWatch,
		StreamID: uint32(1000 + watcherID),
		Body:     map[string]any{"state": *flagKey},
	})
	c.read() // STATE_INIT

	for {
		_, data, err := c.ws.ReadMessage()
		if err != nil {
			return
		}
		f, err := sodp.DecodeFrame(data)
		if err != nil || f.Type != sodp.FrameDelta {
			continue
		}
		totalBytes.Add(int64(len(data)))
		deltaCount.Add(1)
	}
}

// ── main ──────────────────────────────────────────────────────────────────────

func main() {
	flag.Parse()

	wsURL := *flagTarget
	var ts *httptest.Server

	if wsURL == "" {
		// Start in-process Go server.
		srv := sodp.NewServer(
			sodp.WithBackpressureLimit(65536),
			sodp.WithRateLimit(10_000_000),
			sodp.WithMaxSessions(10_000),
			sodp.WithMaxWatches(1024),
		)
		mux := http.NewServeMux()
		mux.HandleFunc("/sodp", srv.HandleWS)
		ts = httptest.NewServer(mux)
		wsURL = "ws" + strings.TrimPrefix(ts.URL, "http") + "/sodp"
		defer ts.Close()
		fmt.Printf("target:      in-process Go server (%s)\n", ts.URL)
	} else {
		fmt.Printf("target:      external server (%s)\n", wsURL)
	}

	fmt.Printf("writers:     %d  (pipeline depth: %d each → %d max in-flight)\n",
		*flagWriters, *flagPipeline, *flagWriters**flagPipeline)
	fmt.Printf("watchers:    %d\n", *flagWatchers)
	fmt.Printf("duration:    %v  (warmup: %v)\n", *flagDuration, *flagWarmup)
	fmt.Printf("key:         %s\n", *flagKey)
	fmt.Printf("────────────────────────────────────────────────────────────\n")

	// ── connect watchers ────────────────────────────────────────────────────
	fmt.Printf("connecting %d watchers...", *flagWatchers)
	watchCtx, watchCancel := context.WithCancel(context.Background())
	var totalWatcherBytes atomic.Int64
	var totalDeltaCount atomic.Int64

	var wwg sync.WaitGroup
	for i := 0; i < *flagWatchers; i++ {
		wwg.Add(1)
		go func(id int) {
			defer wwg.Done()
			runWatcher(watchCtx, wsURL, id, &totalWatcherBytes, &totalDeltaCount)
		}(i)
	}
	// Give watchers time to subscribe.
	time.Sleep(200 * time.Millisecond)
	fmt.Printf(" ok\n")

	// ── warmup ─────────────────────────────────────────────────────────────
	fmt.Printf("warming up for %v...", *flagWarmup)
	writeCtx, writeCancel := context.WithCancel(context.Background())
	var recording atomic.Bool
	var ops atomic.Int64
	st := &stats{}

	var wg sync.WaitGroup
	for i := 0; i < *flagWriters; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			runWriter(writeCtx, wsURL, id, &recording, st, &ops)
		}(i)
	}
	time.Sleep(*flagWarmup)
	fmt.Printf(" ok\n\n")

	// ── measured run ────────────────────────────────────────────────────────
	recording.Store(true)
	totalWatcherBytes.Store(0)
	totalDeltaCount.Store(0)

	start := time.Now()
	ticker := time.NewTicker(*flagReport)
	deadline := time.NewTimer(*flagDuration)

	var lastOps int64
	var lastTime = start

	fmt.Printf("%-8s  %-18s  %-10s  %-10s  %-10s  %-10s  %-10s  %-12s\n",
		"elapsed", "throughput", "P50", "P95", "P99", "P999", "goroutines", "heap")
	fmt.Printf("%s\n", strings.Repeat("─", 95))

	running := true
	for running {
		select {
		case t := <-ticker.C:
			snap := st.snapshot()
			sort.Slice(snap, func(i, j int) bool { return snap[i] < snap[j] })
			curOps := ops.Load()
			interval := t.Sub(lastTime).Seconds()
			throughput := float64(curOps-lastOps) / interval
			lastOps = curOps
			lastTime = t

			var mem runtime.MemStats
			runtime.ReadMemStats(&mem)
			heapMB := float64(mem.HeapInuse) / (1024 * 1024)

			fmt.Printf("%-8s  %-18s  %-10s  %-10s  %-10s  %-10s  %-10d  %.1f MB\n",
				time.Since(start).Round(time.Second),
				commaf(int(throughput))+" ops/sec",
				formatDur(pct(snap, 50)),
				formatDur(pct(snap, 95)),
				formatDur(pct(snap, 99)),
				formatDur(pct(snap, 99.9)),
				runtime.NumGoroutine(),
				heapMB,
			)

		case <-deadline.C:
			running = false
		}
	}

	elapsed := time.Since(start)
	recording.Store(false)
	writeCancel()
	wg.Wait()

	watchCancel()
	wwg.Wait()

	// ── final report ────────────────────────────────────────────────────────
	finalOps := ops.Load()
	finalBytes := totalWatcherBytes.Load()
	finalDeltas := totalDeltaCount.Load()

	printSummary(
		"\nFinal results",
		st.snapshot(),
		elapsed,
		finalOps,
		finalBytes,
	)

	// Delivery check: each mutation should produce 1 DELTA per watcher.
	// Deltas may lag slightly (watcher reads may not have finished draining).
	expectedDeltas := finalOps * int64(*flagWatchers)
	deliveryPct := float64(0)
	if expectedDeltas > 0 {
		deliveryPct = float64(finalDeltas) / float64(expectedDeltas) * 100
	}
	fmt.Printf("  delta delivery: %s / %s (%.1f%% — expect <100%% if watchers lagged)\n",
		commaf(int(finalDeltas)), commaf(int(expectedDeltas)), deliveryPct)

	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)
	fmt.Printf("  heap (final):  %.1f MB  (sys: %.1f MB)\n",
		float64(mem.HeapInuse)/(1024*1024),
		float64(mem.Sys)/(1024*1024),
	)
	fmt.Println()
}
