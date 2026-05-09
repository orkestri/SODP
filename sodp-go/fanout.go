package sodp

import (
	"sync"
	"time"

	"github.com/vmihailenco/msgpack/v5"
)

const fanoutIdleTimeout = 5 * time.Minute

// appendMsgpackUint appends the minimal-width msgpack encoding of v to b.
// Used by Broadcast and writePump to hand-build DELTA frame headers without
// calling msgpack.Marshal for the outer 4-element array.
func appendMsgpackUint(b []byte, v uint64) []byte {
	switch {
	case v <= 0x7f:
		return append(b, byte(v))
	case v <= 0xff:
		return append(b, 0xcc, byte(v))
	case v <= 0xffff:
		return append(b, 0xcd, byte(v>>8), byte(v))
	case v <= 0xffffffff:
		return append(b, 0xce, byte(v>>24), byte(v>>16), byte(v>>8), byte(v))
	default:
		return append(b, 0xcf,
			byte(v>>56), byte(v>>48), byte(v>>40), byte(v>>32),
			byte(v>>24), byte(v>>16), byte(v>>8), byte(v))
	}
}

// outMsg is the message type for a session's Send channel.
//
// For RESULT, STATE_INIT, ERROR, HEARTBEAT, AUTH_OK frames: raw is set to the
// pre-encoded msgpack bytes.
//
// For DELTA fanout frames: raw is nil; streamID and tail are set. The
// writePump assembles the final frame bytes, distributing that O(1) work
// across N write goroutines instead of doing it once sequentially in the
// fanout goroutine.
type outMsg struct {
	raw      []byte // non-nil: pre-encoded frame
	streamID uint32 // fanout only: subscriber's WATCH stream_id
	tail     []byte // fanout only: [seq_bytes | body_bytes], shared across all subscribers
}

// fanoutJob is enqueued by Broadcast and consumed by the per-key fanout goroutine.
type fanoutJob struct {
	delta           DeltaEntry
	senderSessionID string
}

// Subscriber represents one active WATCH subscription.
type Subscriber struct {
	SessionID string
	StreamID  uint32
	Send      chan<- outMsg
	Enqueue   func() // schedules the session for writing; may be nil in tests
}

// FanoutBus manages per-key subscriber lists and fan-out delivery.
//
// Each state key gets its own goroutine that runs asynchronously, so
// Broadcast returns immediately and never blocks the calling readPump.
// The per-subscriber frame (stream_id prefix + shared tail) is assembled
// inside the writePump goroutine, distributing that work across N goroutines.
// Idle per-key goroutines reap themselves after fanoutIdleTimeout.
type FanoutBus struct {
	mu     sync.RWMutex
	subs   map[string][]Subscriber
	keyChs map[string]chan fanoutJob // one buffered channel + goroutine per key
	done   chan struct{}             // closed by Close(); signals all workers to stop
	once   sync.Once
	coll   Collector
}

// NewFanoutBus creates an empty fanout bus.
func NewFanoutBus(coll Collector) *FanoutBus {
	if coll == nil {
		coll = noopCollector{}
	}
	return &FanoutBus{
		subs:   make(map[string][]Subscriber),
		keyChs: make(map[string]chan fanoutJob),
		done:   make(chan struct{}),
		coll:   coll,
	}
}

// Close stops all fanout goroutines. Call this when the server shuts down.
func (fb *FanoutBus) Close() {
	fb.once.Do(func() { close(fb.done) })
}

// Subscribe adds a subscriber for a state key, starting a fanout goroutine
// for the key if one does not exist yet.
func (fb *FanoutBus) Subscribe(key string, sub Subscriber) {
	fb.mu.Lock()
	defer fb.mu.Unlock()
	fb.subs[key] = append(fb.subs[key], sub)
	if _, ok := fb.keyChs[key]; !ok {
		ch := make(chan fanoutJob, 256)
		fb.keyChs[key] = ch
		go fb.fanoutWorker(key, ch)
	}
}

// Unsubscribe removes a subscriber by session and stream ID.
func (fb *FanoutBus) Unsubscribe(key, sessionID string, streamID uint32) {
	fb.mu.Lock()
	defer fb.mu.Unlock()
	subs := fb.subs[key]
	for i, s := range subs {
		if s.SessionID == sessionID && s.StreamID == streamID {
			subs[i] = subs[len(subs)-1]
			fb.subs[key] = subs[:len(subs)-1]
			return
		}
	}
}

// RemoveSession purges all subscriptions for a disconnected session.
func (fb *FanoutBus) RemoveSession(sessionID string) {
	fb.mu.Lock()
	defer fb.mu.Unlock()
	for key, subs := range fb.subs {
		out := subs[:0]
		for _, s := range subs {
			if s.SessionID != sessionID {
				out = append(out, s)
			}
		}
		if len(out) == 0 {
			delete(fb.subs, key)
		} else {
			fb.subs[key] = out
		}
	}
}

// Broadcast enqueues a delta for async fan-out to all subscribers of delta.Key
// except the sender. It returns immediately — the actual delivery happens in the
// key's dedicated goroutine, so the readPump is never blocked by subscriber count.
//
// If the fanout queue is full (the fanout goroutine is behind), the delta is
// dropped for that key. Clients that miss a delta will recover via RESUME.
func (fb *FanoutBus) Broadcast(delta DeltaEntry, senderSessionID string) {
	fb.mu.RLock()
	ch := fb.keyChs[delta.Key]
	fb.mu.RUnlock()
	if ch == nil {
		return
	}
	select {
	case ch <- fanoutJob{delta: delta, senderSessionID: senderSessionID}:
	case <-fb.done:
	default: // queue full — watcher misses delta; RESUME recovers
		fb.coll.DeltaDropped("fanout")
	}
}

// SubscriberCount returns the number of active watchers for a key.
func (fb *FanoutBus) SubscriberCount(key string) int {
	fb.mu.RLock()
	defer fb.mu.RUnlock()
	return len(fb.subs[key])
}

// fanoutWorker is the per-key goroutine. It encodes the delta body once and
// sends a shared tail reference to each subscriber's writePump channel.
// After fanoutIdleTimeout of no activity it reaps itself from keyChs.
func (fb *FanoutBus) fanoutWorker(key string, ch <-chan fanoutJob) {
	timer := time.NewTimer(fanoutIdleTimeout)
	defer timer.Stop()
	for {
		select {
		case job := <-ch:
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
			timer.Reset(fanoutIdleTimeout)
			fb.deliver(job)
		case <-timer.C:
			// Idle — try to reap: acquire write lock and remove from keyChs.
			fb.mu.Lock()
			if len(ch) == 0 {
				delete(fb.keyChs, key)
				fb.mu.Unlock()
				return
			}
			fb.mu.Unlock()
			timer.Reset(fanoutIdleTimeout)
		case <-fb.done:
			return
		}
	}
}

// deliver encodes the delta body once, builds the shared tail, then sends an
// outMsg to each subscriber's channel. The writePump goroutine for each
// subscriber prepends its own stream_id bytes — O(1) work per subscriber,
// done in parallel across N writePumps.
func (fb *FanoutBus) deliver(job fanoutJob) {
	bodyBytes, err := msgpack.Marshal(job.delta)
	if err != nil {
		return
	}
	// tail = [seq_bytes | body_bytes] — constant across all subscribers for this mutation
	tail := appendMsgpackUint(nil, job.delta.Version)
	tail = append(tail, bodyBytes...)

	fb.mu.RLock()
	subs := fb.subs[job.delta.Key]
	snapshot := make([]Subscriber, len(subs))
	copy(snapshot, subs)
	fb.mu.RUnlock()

	for _, sub := range snapshot {
		if sub.SessionID == job.senderSessionID {
			continue
		}
		// Send a pointer to the shared tail; no per-subscriber allocation here.
		select {
		case sub.Send <- outMsg{streamID: sub.StreamID, tail: tail}:
			if sub.Enqueue != nil {
				sub.Enqueue()
			}
			fb.coll.DeltaDelivered()
		default: // slow subscriber — drop; RESUME recovers
			fb.coll.DeltaDropped("session")
		}
	}
}
