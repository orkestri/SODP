// loadtest benchmarks the sodp-go library in two scenarios:
//
//  1. Solo — 1 writer, 0 watchers: raw CALL→RESULT throughput and latency.
//  2. Fanout — 1 writer, N watchers: per-watcher latency distribution from
//     the moment the CALL is sent to the moment each watcher receives its DELTA.
//
// Usage:
//
//	go run ./cmd/loadtest
//	go run ./cmd/loadtest -mutations=5000 -watchers=100
//	go run ./cmd/loadtest -mutations=5000 -watchers=1000
package main

import (
	"flag"
	"fmt"
	"log"
	"math"
	"net/http"
	"net/http/httptest"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	sodp "github.com/orkestri/sodp-go"
)

var (
	flagMutations = flag.Int("mutations", 2000, "number of mutations to measure")
	flagWarmup    = flag.Int("warmup", 200, "warmup iterations (not counted)")
	flagWatchers  = flag.Int("watchers", 100, "concurrent watchers for fanout test")
	flagKey       = flag.String("key", "bench", "state key")
)

// ── low-level helpers ─────────────────────────────────────────────────────────

type wconn struct{ ws *websocket.Conn }

func dialConn(ts *httptest.Server) *wconn {
	url := "ws" + strings.TrimPrefix(ts.URL, "http") + "/sodp"
	ws, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		log.Fatalf("dial: %v", err)
	}
	return &wconn{ws}
}

func (c *wconn) send(f sodp.Frame) {
	data, err := sodp.EncodeFrame(f)
	if err != nil {
		log.Fatalf("encode: %v", err)
	}
	if err := c.ws.WriteMessage(websocket.BinaryMessage, data); err != nil {
		log.Fatalf("write: %v", err)
	}
}

func (c *wconn) read() (sodp.Frame, int) {
	c.ws.SetReadDeadline(time.Now().Add(30 * time.Second))
	_, data, err := c.ws.ReadMessage()
	if err != nil {
		log.Fatalf("read: %v", err)
	}
	f, err := sodp.DecodeFrame(data)
	if err != nil {
		log.Fatalf("decode: %v", err)
	}
	return f, len(data)
}

func (c *wconn) close() { c.ws.Close() }

func callFrame(i int, value map[string]any) sodp.Frame {
	return sodp.Frame{
		Type:     sodp.FrameCall,
		StreamID: 1,
		Seq:      uint64(i),
		Body: map[string]any{
			"call_id": fmt.Sprintf("c%d", i),
			"method":  "state.set",
			"args":    map[string]any{"state": *flagKey, "value": value},
		},
	}
}

// ── statistics ────────────────────────────────────────────────────────────────

func p(sorted []time.Duration, pct float64) time.Duration {
	if len(sorted) == 0 {
		return 0
	}
	idx := int(math.Ceil(pct/100*float64(len(sorted)))) - 1
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

func printStats(label string, latencies []time.Duration, elapsed time.Duration, bytesPerRound int64, nWatchers int) {
	s := make([]time.Duration, len(latencies))
	copy(s, latencies)
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })

	n := len(s)
	// For fanout, each mutation produces nWatchers latency samples.
	// "mutations measured" = n / nWatchers (if nWatchers > 0).
	mutations := n
	if nWatchers > 1 {
		mutations = n / nWatchers
	}
	throughput := float64(mutations) / elapsed.Seconds()

	fmt.Printf("\n%s\n", label)
	fmt.Printf("  mutations:   %d\n", mutations)
	fmt.Printf("  elapsed:     %v\n", elapsed.Round(time.Millisecond))
	fmt.Printf("  throughput:  %.0f ops/sec\n", throughput)
	fmt.Printf("  P50 latency: %v\n", p(s, 50).Round(time.Microsecond))
	fmt.Printf("  P95 latency: %v\n", p(s, 95).Round(time.Microsecond))
	fmt.Printf("  P99 latency: %v\n", p(s, 99).Round(time.Microsecond))
	if bytesPerRound > 0 {
		fmt.Printf("  bytes/round: %d  (~%d/watcher)\n", bytesPerRound, bytesPerRound/int64(nWatchers))
	}
}

// ── Scenario 1: solo (CALL → RESULT) ─────────────────────────────────────────

func benchSolo(ts *httptest.Server) {
	c := dialConn(ts)
	defer c.close()
	c.read() // HELLO

	total := *flagWarmup + *flagMutations
	value := benchValue()

	for i := 0; i < *flagWarmup; i++ {
		value["counter"] = i
		c.send(callFrame(i, value))
		c.read() // RESULT
	}

	latencies := make([]time.Duration, 0, *flagMutations)
	start := time.Now()
	for i := *flagWarmup; i < total; i++ {
		value["counter"] = i
		t0 := time.Now()
		c.send(callFrame(i, value))
		c.read() // RESULT
		latencies = append(latencies, time.Since(t0))
	}
	printStats("Solo (CALL → RESULT, 0 watchers)", latencies, time.Since(start), 0, 1)
}

// ── Scenario 2: fanout (1 writer → N watchers) ───────────────────────────────
//
// The writer sends mutations in a tight loop, reading each RESULT before the
// next CALL. sentAts[i] is recorded immediately before each CALL is sent.
//
// Each watcher runs independently: it reads DELTAs in order (TCP guarantees
// ordering per connection). Since the writer is sequential, watcher's j-th
// DELTA always corresponds to the j-th mutation, so latency[j] = now - sentAts[j].
//
// This gives us per-watcher latency samples — N_watchers × N_mutations of them —
// which is the correct distribution for answering "how long until a watcher gets
// its DELTA?"

func benchFanout(ts *httptest.Server, numWatchers int) {
	total := *flagWarmup + *flagMutations

	// sentAts[i] is written by the writer goroutine before mutation i is sent.
	// Watcher goroutines read sentAts[i] after receiving the i-th DELTA.
	// Since the writer is sequential (waits for RESULT before next CALL), the
	// DELTA for mutation i cannot arrive at a watcher before sentAts[i] is set.
	sentAts := make([]time.Time, total)

	writer := dialConn(ts)
	defer writer.close()
	writer.read() // HELLO

	watchers := make([]*wconn, numWatchers)
	for i := range watchers {
		watchers[i] = dialConn(ts)
		watchers[i].read() // HELLO
		watchers[i].send(sodp.Frame{
			Type:     sodp.FrameWatch,
			StreamID: uint32(100 + i),
			Body:     map[string]any{"state": *flagKey},
		})
		watchers[i].read() // STATE_INIT
	}
	time.Sleep(50 * time.Millisecond) // let subscriptions reach fanout bus

	// Each watcher goroutine collects its own latency samples.
	// We use a flat slice per watcher to avoid any shared lock on the hot path.
	type sample struct {
		lat   time.Duration
		bytes int
	}
	results := make([][]sample, numWatchers)
	for i := range results {
		results[i] = make([]sample, 0, total)
	}

	var wwg sync.WaitGroup
	for wi, w := range watchers {
		wi, w := wi, w
		wwg.Add(1)
		go func() {
			defer wwg.Done()
			for j := 0; j < total; j++ {
				f, nb := w.read()
				recvAt := time.Now()
				if f.Type != sodp.FrameDelta {
					j-- // skip non-delta frames (shouldn't happen after STATE_INIT)
					continue
				}
				if j >= *flagWarmup {
					results[wi] = append(results[wi], sample{
						lat:   recvAt.Sub(sentAts[j]),
						bytes: nb,
					})
				}
			}
		}()
	}

	value := benchValue()
	start := time.Now()
	for i := 0; i < total; i++ {
		value["counter"] = i
		sentAts[i] = time.Now()
		writer.send(callFrame(i, value))
		writer.read() // RESULT — ensures server processed the mutation before we advance
	}
	elapsed := time.Since(start)

	wwg.Wait()
	for _, w := range watchers {
		w.close()
	}

	// Flatten all watcher latency samples.
	latencies := make([]time.Duration, 0, *flagMutations*numWatchers)
	var totalBytes int64
	for _, rs := range results {
		for _, s := range rs {
			latencies = append(latencies, s.lat)
			totalBytes += int64(s.bytes)
		}
	}

	bytesPerRound := totalBytes * int64(numWatchers) / int64(len(latencies))

	label := fmt.Sprintf("Fanout (1 writer → %d watchers)", numWatchers)
	printStats(label, latencies, elapsed, bytesPerRound, numWatchers)
}

func benchValue() map[string]any {
	return map[string]any{
		"status":  "running",
		"counter": 0,
		"node":    "bench-node",
		"score":   3.14,
		"active":  true,
		"label":   "iteration",
		"meta":    map[string]any{"source": "loadtest", "version": 1},
	}
}

// ── main ──────────────────────────────────────────────────────────────────────

func main() {
	flag.Parse()

	srv := sodp.NewServer(
		sodp.WithBackpressureLimit(8192),
		sodp.WithRateLimit(1_000_000),
		sodp.WithMaxSessions(10_000),
		sodp.WithMaxWatches(1024),
	)
	mux := http.NewServeMux()
	mux.HandleFunc("/sodp", srv.HandleWS)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	fmt.Printf("sodp-go load test\n")
	fmt.Printf("mutations: %d  warmup: %d  watchers: %d\n",
		*flagMutations, *flagWarmup, *flagWatchers)
	fmt.Printf("─────────────────────────────────────────────────────")

	benchSolo(ts)
	if *flagWatchers > 0 {
		benchFanout(ts, *flagWatchers)
	}

	fmt.Println()
}
