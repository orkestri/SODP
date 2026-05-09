package sodp

// StateEntry is a single key/version/value triple returned by ClusterBackend.LoadAll.
type StateEntry struct {
	Key     string
	Version uint64
	Value   any
}

// ClusterBackend abstracts the distributed state store and pub/sub transport
// used for horizontal scaling. Implementations must be safe for concurrent use.
type ClusterBackend interface {
	// LoadAll returns a snapshot of all keys from the shared store.
	// Called once at startup to synchronise local state with the cluster.
	LoadAll() ([]StateEntry, error)

	// SyncState propagates a state mutation to the shared store (fire-and-forget).
	SyncState(key string, version uint64, value any) error

	// SyncDelete removes a key from the shared store (fire-and-forget).
	SyncDelete(key string) error

	// PublishDelta broadcasts a delta to all other nodes via pub/sub.
	PublishDelta(delta DeltaEntry) error

	// Subscribe registers fn to be called for every delta received from other
	// nodes. The implementation must skip deltas published by this node.
	Subscribe(fn func(DeltaEntry)) error

	// NodeID returns the unique identifier for this server instance.
	NodeID() string

	// Close releases all resources held by the backend.
	Close() error
}
