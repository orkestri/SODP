package sodpredis

import (
	"context"
	"fmt"
	"log"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/orkestri/sodp-go"
	"github.com/redis/go-redis/v9"
	"github.com/vmihailenco/msgpack/v5"
)

const (
	stateHashKey    = "sodp:state"
	deltaChannelPfx = "sodp:delta:"
	subscribePat    = "sodp:delta:*"
	reconnectDelay  = 2 * time.Second
)

// Cluster implements sodp.ClusterBackend backed by Redis.
type Cluster struct {
	client *redis.Client
	nodeID string
	ctx    context.Context
	cancel context.CancelFunc
	fn     func(sodp.DeltaEntry)
}

// New parses redisURL, creates a Redis client, and returns a ready Cluster.
func New(redisURL string) (*Cluster, error) {
	opts, err := redis.ParseURL(redisURL)
	if err != nil {
		return nil, fmt.Errorf("sodpredis: parse url: %w", err)
	}
	client := redis.NewClient(opts)
	ctx, cancel := context.WithCancel(context.Background())

	c := &Cluster{
		client: client,
		nodeID: uuid.New().String(),
		ctx:    ctx,
		cancel: cancel,
	}
	return c, nil
}

// LoadAll fetches the full state hash from Redis and decodes each entry.
func (c *Cluster) LoadAll() ([]sodp.StateEntry, error) {
	result, err := c.client.HGetAll(c.ctx, stateHashKey).Result()
	if err != nil {
		return nil, fmt.Errorf("sodpredis: HGETALL: %w", err)
	}

	entries := make([]sodp.StateEntry, 0, len(result))
	for key, raw := range result {
		var pair []any
		if err := msgpack.Unmarshal([]byte(raw), &pair); err != nil || len(pair) != 2 {
			log.Printf("sodpredis: LoadAll: skip malformed entry for key %q: %v", key, err)
			continue
		}
		version, ok := toUint64(pair[0])
		if !ok {
			log.Printf("sodpredis: LoadAll: skip entry with non-numeric version for key %q", key)
			continue
		}
		entries = append(entries, sodp.StateEntry{
			Key:     key,
			Version: version,
			Value:   pair[1],
		})
	}
	return entries, nil
}

// SyncState marshals [version, value] and stores it in the Redis state hash.
// The write runs in a background goroutine so the caller is never blocked.
func (c *Cluster) SyncState(key string, version uint64, value any) error {
	b, err := msgpack.Marshal([]any{version, value})
	if err != nil {
		return fmt.Errorf("sodpredis: SyncState marshal: %w", err)
	}
	go func() {
		if err := c.client.HSet(c.ctx, stateHashKey, key, b).Err(); err != nil {
			log.Printf("sodpredis: SyncState HSET %q: %v", key, err)
		}
	}()
	return nil
}

// SyncDelete removes a key from the Redis state hash (fire-and-forget).
func (c *Cluster) SyncDelete(key string) error {
	go func() {
		if err := c.client.HDel(c.ctx, stateHashKey, key).Err(); err != nil {
			log.Printf("sodpredis: SyncDelete HDEL %q: %v", key, err)
		}
	}()
	return nil
}

// PublishDelta marshals [nodeID, delta] and publishes it to the per-key channel.
func (c *Cluster) PublishDelta(delta sodp.DeltaEntry) error {
	b, err := msgpack.Marshal([]any{c.nodeID, delta})
	if err != nil {
		return fmt.Errorf("sodpredis: PublishDelta marshal: %w", err)
	}
	channel := deltaChannelPfx + delta.Key
	if err := c.client.Publish(c.ctx, channel, b).Err(); err != nil {
		return fmt.Errorf("sodpredis: PUBLISH %q: %w", channel, err)
	}
	return nil
}

// Subscribe sets fn as the delta handler and starts the background PSubscribe loop.
// Only one subscriber per Cluster is supported; subsequent calls replace the handler.
func (c *Cluster) Subscribe(fn func(sodp.DeltaEntry)) error {
	c.fn = fn
	go c.subscribeLoop()
	return nil
}

// NodeID returns the UUID assigned to this server instance.
func (c *Cluster) NodeID() string {
	return c.nodeID
}

// Close cancels the context and closes the Redis client.
func (c *Cluster) Close() error {
	c.cancel()
	return c.client.Close()
}

// subscribeLoop runs PSubscribe in a retry loop. On error it waits reconnectDelay
// before reconnecting so transient Redis restarts don't terminate the goroutine.
func (c *Cluster) subscribeLoop() {
	for {
		select {
		case <-c.ctx.Done():
			return
		default:
		}

		if err := c.runSubscribe(); err != nil {
			if c.ctx.Err() != nil {
				return
			}
			log.Printf("sodpredis: subscriber error: %v — reconnecting in %s", err, reconnectDelay)
			select {
			case <-c.ctx.Done():
				return
			case <-time.After(reconnectDelay):
			}
		}
	}
}

// runSubscribe opens a PSubscribe connection and processes messages until an
// error occurs or the context is cancelled.
func (c *Cluster) runSubscribe() error {
	pubsub := c.client.PSubscribe(c.ctx, subscribePat)
	defer pubsub.Close()

	ch := pubsub.Channel()
	for {
		select {
		case <-c.ctx.Done():
			return nil
		case msg, ok := <-ch:
			if !ok {
				return fmt.Errorf("channel closed")
			}
			c.handleMessage(msg)
		}
	}
}

// handleMessage decodes a pub/sub message and invokes the registered callback.
// Messages published by this node are silently skipped.
func (c *Cluster) handleMessage(msg *redis.Message) {
	var pair []any
	if err := msgpack.Unmarshal([]byte(msg.Payload), &pair); err != nil || len(pair) != 2 {
		log.Printf("sodpredis: handleMessage: malformed payload on %q: %v", msg.Channel, err)
		return
	}

	senderID, _ := pair[0].(string)
	if senderID == c.nodeID {
		return
	}

	// Re-encode the delta portion and decode into DeltaEntry so that
	// msgpack struct tags are applied correctly.
	deltaBytes, err := msgpack.Marshal(pair[1])
	if err != nil {
		log.Printf("sodpredis: handleMessage: re-marshal delta: %v", err)
		return
	}
	var delta sodp.DeltaEntry
	if err := msgpack.Unmarshal(deltaBytes, &delta); err != nil {
		log.Printf("sodpredis: handleMessage: unmarshal delta: %v", err)
		return
	}

	// If the key was not set in the delta body, derive it from the channel name.
	if delta.Key == "" {
		delta.Key = strings.TrimPrefix(msg.Channel, deltaChannelPfx)
	}

	if c.fn != nil {
		c.fn(delta)
	}
}

// toUint64 converts numeric msgpack-decoded values to uint64.
func toUint64(v any) (uint64, bool) {
	switch n := v.(type) {
	case uint64:
		return n, true
	case int64:
		return uint64(n), true
	case float64:
		return uint64(n), true
	case uint8:
		return uint64(n), true
	case uint16:
		return uint64(n), true
	case uint32:
		return uint64(n), true
	case int8:
		return uint64(n), true
	case int16:
		return uint64(n), true
	case int32:
		return uint64(n), true
	}
	return 0, false
}
