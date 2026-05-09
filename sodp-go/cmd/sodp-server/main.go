// sodp-server is a standalone SODP WebSocket server for benchmarking and
// integration testing. Start it, then point cmd/bench at it with -target.
//
// Usage:
//
//	go run ./cmd/sodp-server
//	go run ./cmd/sodp-server -addr :7778
package main

import (
	"flag"
	"log"
	"net/http"

	sodp "github.com/orkestri/sodp-go"
)

var (
	flagAddr        = flag.String("addr", ":7777", "listen address")
	flagBackpressure = flag.Int("backpressure", 256, "per-session send buffer (messages)")
	flagRateLimit   = flag.Int("rate", 1_000_000, "write rate limit per session (ops/sec)")
	flagMaxSessions = flag.Int("max-sessions", 10_000, "max concurrent sessions")
)

func main() {
	flag.Parse()

	srv := sodp.NewServer(
		sodp.WithBackpressureLimit(*flagBackpressure),
		sodp.WithRateLimit(*flagRateLimit),
		sodp.WithMaxSessions(*flagMaxSessions),
	)
	mux := http.NewServeMux()
	mux.HandleFunc("/sodp", srv.HandleWS)
	mux.HandleFunc("/", srv.HandleWS) // bare-path for bench -target ws://host:port

	log.Printf("sodp-go server listening on %s", *flagAddr)
	if err := http.ListenAndServe(*flagAddr, mux); err != nil {
		log.Fatal(err)
	}
}
