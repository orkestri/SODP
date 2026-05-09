package sodp

import (
	"sync"
	"sync/atomic"
	"time"
)

const (
	defaultSessionBuffer = 128
	defaultRateLimit     = 100
	defaultMaxWatches    = 64
	firstStreamID        = 10
)

// Session represents one authenticated WebSocket connection.
// Fields are safe to read after authentication; mutate only via methods.
type Session struct {
	ID     string
	Sub    string         // JWT "sub" claim; empty if auth is disabled
	Claims map[string]any // all JWT claims

	Send chan []byte   // outbound pre-encoded frames (buffered)
	Done chan struct{} // closed on disconnect

	mu         sync.Mutex
	watches    map[uint32]string // stream_id → key
	nextStream atomic.Uint32
	authed     bool
	maxWatches int

	rateCount atomic.Int64
	rateReset atomic.Int64
	rateLimit int
}

// NewSession creates a session for a new connection.
func NewSession(id string) *Session {
	s := &Session{
		ID:         id,
		Claims:     make(map[string]any),
		Send:       make(chan []byte, defaultSessionBuffer),
		Done:       make(chan struct{}),
		watches:    make(map[uint32]string),
		rateLimit:  defaultRateLimit,
		maxWatches: defaultMaxWatches,
	}
	s.nextStream.Store(firstStreamID)
	return s
}

// AllocStream returns the next monotonic stream ID.
func (s *Session) AllocStream() uint32 {
	return s.nextStream.Add(1) - 1
}

// AddWatch records a WATCH subscription. Returns false if the watch limit is reached.
func (s *Session) AddWatch(streamID uint32, key string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.watches) >= s.maxWatches {
		return false
	}
	s.watches[streamID] = key
	return true
}

// RemoveWatch removes a subscription by stream ID, returning the key.
func (s *Session) RemoveWatch(streamID uint32) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	key, ok := s.watches[streamID]
	if ok {
		delete(s.watches, streamID)
	}
	return key, ok
}

// StreamForKey returns the stream ID watching a given key, or 0 if not found.
func (s *Session) StreamForKey(key string) uint32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	for sid, k := range s.watches {
		if k == key {
			return sid
		}
	}
	return 0
}

// WatchCount returns the number of active WATCH subscriptions.
func (s *Session) WatchCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.watches)
}

// Watches returns a snapshot of active watch subscriptions.
func (s *Session) Watches() map[uint32]string {
	s.mu.Lock()
	defer s.mu.Unlock()
	cp := make(map[uint32]string, len(s.watches))
	for k, v := range s.watches {
		cp[k] = v
	}
	return cp
}

// CheckRate returns true if the mutation is within the rate limit.
// Uses a fixed 1-second window.
func (s *Session) CheckRate() bool {
	now := time.Now().Unix()
	if now > s.rateReset.Load() {
		s.rateCount.Store(1)
		s.rateReset.Store(now + 1)
		return true
	}
	return s.rateCount.Add(1) <= int64(s.rateLimit)
}

// IsAuthenticated returns whether the session has completed AUTH.
func (s *Session) IsAuthenticated() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.authed
}

// MarkAuthenticated marks the session as authenticated (idempotent).
func (s *Session) MarkAuthenticated() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.authed = true
}

// SetAuth stores authentication details. May only be called once (re-auth is
// blocked by the server after the first successful AUTH).
func (s *Session) SetAuth(sub string, claims map[string]any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Sub = sub
	s.Claims = claims
	s.authed = true
}

// Close signals session termination by closing the Done channel.
func (s *Session) Close() {
	select {
	case <-s.Done:
	default:
		close(s.Done)
	}
}
