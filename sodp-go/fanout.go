package sodp

import (
	"sync"

	"github.com/vmihailenco/msgpack/v5"
)

// appendMsgpackUint appends the minimal msgpack encoding of a uint64 to b.
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

// Subscriber represents one active WATCH subscription from a session.
type Subscriber struct {
	SessionID string
	StreamID  uint32
	Send      chan<- []byte // pre-encoded frame bytes; must be buffered
}

// FanoutBus manages per-key subscriber lists and broadcasts pre-encoded delta
// frames. The delta body is encoded once per mutation; per-subscriber cost is
// only the 4-element frame envelope with the subscriber's stream_id.
type FanoutBus struct {
	mu   sync.RWMutex
	subs map[string][]Subscriber
}

// NewFanoutBus creates an empty fanout bus.
func NewFanoutBus() *FanoutBus {
	return &FanoutBus{subs: make(map[string][]Subscriber)}
}

// Subscribe adds a subscriber for a state key.
func (fb *FanoutBus) Subscribe(key string, sub Subscriber) {
	fb.mu.Lock()
	defer fb.mu.Unlock()
	fb.subs[key] = append(fb.subs[key], sub)
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
		filtered := subs[:0]
		for _, s := range subs {
			if s.SessionID != sessionID {
				filtered = append(filtered, s)
			}
		}
		if len(filtered) == 0 {
			delete(fb.subs, key)
		} else {
			fb.subs[key] = filtered
		}
	}
}

// Broadcast encodes the delta body once, then hand-builds a per-subscriber
// DELTA frame by prepending only the 2–5 byte stream_id field.
// No msgpack.Marshal is called inside the loop — only a slice allocation
// and two copies per subscriber.
// The sender session is skipped; slow subscribers are dropped (non-blocking send).
func (fb *FanoutBus) Broadcast(delta DeltaEntry, senderSessionID string) {
	bodyBytes, err := msgpack.Marshal(delta)
	if err != nil {
		return
	}

	// Pre-build the constant tail: [seq_bytes | body_bytes].
	// fixarray(4) + FrameDelta(0x04) are always 2 bytes; they're prepended inline.
	tail := appendMsgpackUint(nil, delta.Version) // seq
	tail = append(tail, bodyBytes...)

	fb.mu.RLock()
	subs := fb.subs[delta.Key]
	snapshot := make([]Subscriber, len(subs))
	copy(snapshot, subs)
	fb.mu.RUnlock()

	for _, sub := range snapshot {
		if sub.SessionID == senderSessionID {
			continue
		}
		// Build [0x94, 0x04, <streamID>, <tail>] without invoking msgpack.Marshal.
		frame := make([]byte, 0, 2+5+len(tail)) // 5 = max stream_id encoding width
		frame = append(frame, 0x94, byte(FrameDelta))
		frame = appendMsgpackUint(frame, uint64(sub.StreamID))
		frame = append(frame, tail...)
		select {
		case sub.Send <- frame:
		default:
		}
	}
}

// SubscriberCount returns the number of active watchers for a key.
func (fb *FanoutBus) SubscriberCount(key string) int {
	fb.mu.RLock()
	defer fb.mu.RUnlock()
	return len(fb.subs[key])
}
