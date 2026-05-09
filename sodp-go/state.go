package sodp

import (
	"sync"
	"sync/atomic"
)

const (
	defaultDeltaLogCap = 1000
	defaultMaxKeys     = 100_000
)

// stateEntry is a versioned value in the state store.
type stateEntry struct {
	Value   any
	Version uint64
}

// StateStore is a thread-safe, versioned key-value store with a per-key
// delta log for RESUME support. It is the single source of truth for all
// SODP state on this server instance.
type StateStore struct {
	mu      sync.RWMutex
	entries map[string]stateEntry
	deltas  map[string]*ringLog
	global  atomic.Uint64
	logCap  int
	maxKeys int
}

// ringLog is a fixed-capacity circular buffer of DeltaEntry values.
type ringLog struct {
	buf  []DeltaEntry
	head int
	len  int
	cap  int
}

func newRingLog(cap int) *ringLog {
	return &ringLog{buf: make([]DeltaEntry, cap), cap: cap}
}

func (r *ringLog) push(e DeltaEntry) {
	r.buf[r.head] = e
	r.head = (r.head + 1) % r.cap
	if r.len < r.cap {
		r.len++
	}
}

func (r *ringLog) oldest() int {
	if r.len < r.cap {
		return 0
	}
	return r.head
}

func (r *ringLog) scan(fn func(DeltaEntry) bool) {
	start := r.oldest()
	for i := 0; i < r.len; i++ {
		if !fn(r.buf[(start+i)%r.cap]) {
			return
		}
	}
}

func (r *ringLog) oldestVersion() uint64 {
	if r.len == 0 {
		return 0
	}
	return r.buf[r.oldest()].Version
}

// NewStateStore creates an empty state store with default limits.
func NewStateStore() *StateStore {
	return &StateStore{
		entries: make(map[string]stateEntry),
		deltas:  make(map[string]*ringLog),
		logCap:  defaultDeltaLogCap,
		maxKeys: defaultMaxKeys,
	}
}

// Apply atomically mutates a key. It computes the structural diff, increments
// the global version, stores the new value, and appends to the delta log.
// Returns nil if the diff is empty (no-op) or the key cap is reached.
func (s *StateStore) Apply(key string, newValue any) *DeltaEntry {
	s.mu.Lock()
	defer s.mu.Unlock()

	old, exists := s.entries[key]
	if !exists && len(s.entries) >= s.maxKeys {
		return nil
	}

	ops := Diff(old.Value, newValue)
	if len(ops) == 0 {
		return nil
	}

	ver := s.global.Add(1)
	s.entries[key] = stateEntry{Value: newValue, Version: ver}

	entry := DeltaEntry{Key: key, Version: ver, Ops: ops}
	s.appendDelta(key, entry)
	return &entry
}

// Append atomically appends an element to a slice-typed state key.
// It emits an O(1) ADD op at JSON Pointer "/-" (RFC 6901 array append).
// When the slice exceeds maxLen, a REMOVE op for the dropped head is added.
// maxLen ≤ 0 disables trimming.
func (s *StateStore) Append(key string, element any, maxLen int) *DeltaEntry {
	s.mu.Lock()
	defer s.mu.Unlock()

	old := s.entries[key]
	var slice []any
	if old.Value != nil {
		slice, _ = old.Value.([]any)
	}

	newSlice := make([]any, len(slice)+1)
	copy(newSlice, slice)
	newSlice[len(slice)] = element

	ops := []DeltaOp{{Op: OpAdd, Path: "/-", Value: element}}

	if maxLen > 0 && len(newSlice) > maxLen {
		newSlice = newSlice[len(newSlice)-maxLen:]
		ops = append(ops, DeltaOp{Op: OpRemove, Path: "/0"})
	}

	ver := s.global.Add(1)
	s.entries[key] = stateEntry{Value: newSlice, Version: ver}

	entry := DeltaEntry{Key: key, Version: ver, Ops: ops}
	s.appendDelta(key, entry)
	return &entry
}

// Delete removes a key from the store and appends a REMOVE delta.
// Returns nil if the key did not exist.
func (s *StateStore) Delete(key string) *DeltaEntry {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, exists := s.entries[key]; !exists {
		return nil
	}

	ver := s.global.Add(1)
	delete(s.entries, key)

	entry := DeltaEntry{
		Key:     key,
		Version: ver,
		Ops:     []DeltaOp{{Op: OpRemove, Path: "/"}},
	}
	s.appendDelta(key, entry)
	return &entry
}

// Get returns the current value and version for a key.
// Returns nil, 0 if the key does not exist.
func (s *StateStore) Get(key string) (any, uint64) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	e := s.entries[key]
	return e.Value, e.Version
}

// Keys returns all current key names.
func (s *StateStore) Keys() []string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	keys := make([]string, 0, len(s.entries))
	for k := range s.entries {
		keys = append(keys, k)
	}
	return keys
}

// KeyCount returns the number of state keys.
func (s *StateStore) KeyCount() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.entries)
}

// GlobalVersion returns the current global version counter.
func (s *StateStore) GlobalVersion() uint64 {
	return s.global.Load()
}

// Snapshot returns a copy of all entries whose key equals prefix or starts with
// prefix followed by ".". An empty prefix returns all entries.
func (s *StateStore) Snapshot(prefix string) map[string]any {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make(map[string]any)
	for k, v := range s.entries {
		if prefix == "" || k == prefix || (len(k) > len(prefix) && k[:len(prefix)] == prefix && k[len(prefix)] == '.') {
			result[k] = v.Value
		}
	}
	return result
}

// EvictIf removes all keys for which predicate returns true.
// It is safe to call concurrently. Returns the number of keys evicted.
// Use this to implement custom TTL eviction or cleanup logic.
func (s *StateStore) EvictIf(predicate func(key string, value any) bool) int {
	s.mu.Lock()
	defer s.mu.Unlock()

	evicted := 0
	for k, e := range s.entries {
		if predicate(k, e.Value) {
			delete(s.entries, k)
			delete(s.deltas, k)
			evicted++
		}
	}
	return evicted
}

// DeltasSince returns all delta entries for a key with version > sinceVersion.
// Returns nil if the delta log does not cover the requested version (caller
// should fall back to sending a full STATE_INIT).
func (s *StateStore) DeltasSince(key string, sinceVersion uint64) []DeltaEntry {
	s.mu.RLock()
	defer s.mu.RUnlock()

	ring := s.deltas[key]
	if ring == nil || ring.len == 0 {
		return nil
	}
	if ring.oldestVersion() > sinceVersion+1 {
		return nil
	}

	var result []DeltaEntry
	ring.scan(func(d DeltaEntry) bool {
		if d.Version > sinceVersion {
			result = append(result, d)
		}
		return true
	})
	return result
}

// Entries returns all key/version/value triples for compaction snapshots.
func (s *StateStore) Entries() []StateEntry {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make([]StateEntry, 0, len(s.entries))
	for k, e := range s.entries {
		out = append(out, StateEntry{Key: k, Version: e.Version, Value: e.Value})
	}
	return out
}

// LoadEntry stores an entry from a persistence replay or cluster sync.
// It skips the diff and delta log; the version must be newer than what is
// already stored, otherwise the call is a no-op.
func (s *StateStore) LoadEntry(key string, version uint64, value any) {
	s.mu.Lock()
	if cur := s.entries[key]; version > cur.Version {
		s.entries[key] = stateEntry{Value: value, Version: version}
	}
	s.mu.Unlock()
	// Advance global counter so the next Apply produces a higher version.
	for {
		cur := s.global.Load()
		if version <= cur || s.global.CompareAndSwap(cur, version) {
			break
		}
	}
}

// LoadDelete removes a key loaded from the persistence WAL.
// Only takes effect if version is at least as recent as the stored version.
func (s *StateStore) LoadDelete(key string, version uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if cur, ok := s.entries[key]; ok && version >= cur.Version {
		delete(s.entries, key)
		delete(s.deltas, key)
	}
}

// ApplyClusterDelta applies a delta received from another cluster node.
// It patches local in-memory state and appends to the delta log so that
// local watchers can RESUME. Returns false if the entry is stale.
func (s *StateStore) ApplyClusterDelta(entry DeltaEntry) bool {
	s.mu.Lock()
	defer s.mu.Unlock()

	cur := s.entries[entry.Key]
	if entry.Version <= cur.Version {
		return false // stale — already at a newer version
	}
	newValue := ApplyOps(cur.Value, entry.Ops)
	s.entries[entry.Key] = stateEntry{Value: newValue, Version: entry.Version}
	s.appendDelta(entry.Key, entry)

	for {
		c := s.global.Load()
		if entry.Version <= c || s.global.CompareAndSwap(c, entry.Version) {
			break
		}
	}
	return true
}

func (s *StateStore) appendDelta(key string, entry DeltaEntry) {
	ring := s.deltas[key]
	if ring == nil {
		ring = newRingLog(s.logCap)
		s.deltas[key] = ring
	}
	ring.push(entry)
}
