// sodp-server is a standalone SODP WebSocket server.
//
// Usage:
//
//	go run ./cmd/sodp-server
//	go run ./cmd/sodp-server -addr :7778 -acl config/acl.json
//
// All flags can be overridden by the corresponding SODP_* environment variable.
package main

import (
	"context"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	sodp "github.com/orkestri/sodp-go"
)

var (
	flagAddr         = flag.String("addr", env("SODP_ADDR", ":7777"), "listen address")
	flagBackpressure = flag.Int("backpressure", 256, "per-session send buffer (messages)")
	flagRateLimit    = flag.Int("rate", 1_000_000, "write rate limit per session (ops/sec)")
	flagMaxSessions  = flag.Int("max-sessions", 10_000, "max concurrent sessions")
	flagMaxWatches   = flag.Int("max-watches", 1024, "max WATCH subscriptions per session")
	flagACL          = flag.String("acl", env("SODP_ACL_FILE", ""), "ACL JSON rules file path")
	flagPersist      = flag.String("persist", env("SODP_PERSIST_DIR", ""), "persistence directory (WAL + snapshot)")
	flagJWTSecret    = flag.String("jwt-secret", env("SODP_JWT_SECRET", ""), "HS256 JWT shared secret")
	flagJWTPubKey    = flag.String("jwt-public-key", env("SODP_JWT_PUBLIC_KEY_FILE", ""), "RS256 public key PEM file path")
	flagHealthPort   = flag.String("health-port", env("SODP_HEALTH_PORT", ""), "health-check HTTP port (empty = disabled)")
)

func env(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func main() {
	flag.Parse()

	var opts []sodp.ServerOption
	opts = append(opts,
		sodp.WithBackpressureLimit(*flagBackpressure),
		sodp.WithRateLimit(*flagRateLimit),
		sodp.WithMaxSessions(*flagMaxSessions),
		sodp.WithMaxWatches(*flagMaxWatches),
	)

	if *flagACL != "" {
		opts = append(opts, sodp.WithACLFile(*flagACL))
	}
	if *flagPersist != "" {
		opts = append(opts, sodp.WithPersistenceDir(*flagPersist))
	}
	if *flagJWTPubKey != "" {
		pemData, err := os.ReadFile(*flagJWTPubKey)
		if err != nil {
			log.Fatalf("sodp: read JWT public key: %v", err)
		}
		opts = append(opts, sodp.WithJWTPublicKey(string(pemData)))
	} else if *flagJWTSecret != "" {
		opts = append(opts, sodp.WithJWTSecret([]byte(*flagJWTSecret)))
	}

	srv := sodp.NewServer(opts...)

	mux := http.NewServeMux()
	mux.HandleFunc("/sodp", srv.HandleWS)
	mux.HandleFunc("/", srv.HandleWS) // bare path for bench -target ws://host:port

	if *flagHealthPort != "" {
		go func() {
			hMux := http.NewServeMux()
			hMux.Handle("/health", srv.HealthHandler())
			addr := ":" + *flagHealthPort
			log.Printf("sodp-go: health endpoint on %s", addr)
			if err := http.ListenAndServe(addr, hMux); err != nil {
				log.Printf("sodp-go: health server: %v", err)
			}
		}()
	}

	httpSrv := &http.Server{Addr: *flagAddr, Handler: mux}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	go func() {
		log.Printf("sodp-go: listening on %s", *flagAddr)
		if err := httpSrv.ListenAndServe(); err != http.ErrServerClosed {
			log.Printf("sodp-go: ListenAndServe: %v", err)
		}
	}()

	<-ctx.Done()
	log.Printf("sodp-go: shutting down…")

	shutCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := httpSrv.Shutdown(shutCtx); err != nil {
		log.Printf("sodp-go: shutdown: %v", err)
	}
	srv.Close()
	log.Printf("sodp-go: shutdown complete")
}
