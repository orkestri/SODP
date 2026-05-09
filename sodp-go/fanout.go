package sodp

import (
	"sync"

	"github.com/vmihailenco/msgpack/v5"
)

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

// Broadcast encodes a delta body once, then composes a DELTA frame per
// subscriber with their specific stream_id. The sender session is skipped
// (the server writes its own DELTA directly on the hot path).
// Subscribers whose channel is full are skipped (slow-client protection).
func (fb *FanoutBus) Broadcast(delta DeltaEntry, senderSessionID string) {
	bodyBytes, err := msgpack.Marshal(delta)
	if err != nil {
		return
	}
	rawBody := msgpack.RawMessage(bodyBytes)

	fb.mu.RLock()
	subs := fb.subs[delta.Key]
	snapshot := make([]Subscriber, len(subs))
	copy(snapshot, subs)
	fb.mu.RUnlock()

	for _, sub := range snapshot {
		if sub.SessionID == senderSessionID {
			continue
		}
		frame, err := msgpack.Marshal([]any{
			uint8(FrameDelta),
			sub.StreamID,
			delta.Version,
			rawBody,
		})
		if err != nil {
			continue
		}
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
