package sodp

import (
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"sync"

	"github.com/vmihailenco/msgpack/v5"
)

const (
	walFile        = "sodp.wal"
	snapFile       = "sodp.snap"
	compactAt      = 10_000 // WAL entries before auto-compaction is triggered
)

// Persist provides crash-safe state durability using a msgpack WAL + snapshot.
//
// Layout inside dir/:
//
//	sodp.snap  — full state snapshot (msgpack array of [key, version, value] triples)
//	sodp.wal   — append-only write-ahead log (msgpack array records)
type Persist struct {
	mu    sync.Mutex
	dir   string
	wal   *os.File
	enc   *msgpack.Encoder
	count int
}

// openPersist opens (or creates) the persistence directory and WAL.
// It does NOT replay history — call LoadAll for that before serving traffic.
func openPersist(dir string) (*Persist, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("sodp: persist: mkdir: %w", err)
	}
	f, err := os.OpenFile(filepath.Join(dir, walFile), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, fmt.Errorf("sodp: persist: open wal: %w", err)
	}
	count := countWAL(filepath.Join(dir, walFile))
	return &Persist{dir: dir, wal: f, enc: msgpack.NewEncoder(f), count: count}, nil
}

// Append writes a set-operation record to the WAL.
func (p *Persist) Append(key string, version uint64, value any) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if err := p.enc.Encode([]any{"s", key, version, value}); err != nil {
		log.Printf("sodp: persist: wal append: %v", err)
		return
	}
	p.count++
}

// AppendDelete writes a delete-operation record to the WAL.
func (p *Persist) AppendDelete(key string, version uint64) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if err := p.enc.Encode([]any{"d", key, version}); err != nil {
		log.Printf("sodp: persist: wal delete: %v", err)
		return
	}
	p.count++
}

// NeedsCompaction reports whether the WAL has grown past the threshold.
func (p *Persist) NeedsCompaction() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.count >= compactAt
}

// Compact writes a fresh snapshot from the store and truncates the WAL.
// It is safe to call concurrently with Append/AppendDelete.
func (p *Persist) Compact(store *StateStore) {
	entries := store.Entries()

	// Build snapshot: msgpack array of [key, version, value] triples.
	snap := make([]any, 0, len(entries)*3)
	for _, e := range entries {
		snap = append(snap, e.Key, e.Version, e.Value)
	}
	data, err := msgpack.Marshal(snap)
	if err != nil {
		log.Printf("sodp: persist: compact marshal: %v", err)
		return
	}
	tmp := filepath.Join(p.dir, snapFile+".tmp")
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		log.Printf("sodp: persist: compact write snap: %v", err)
		return
	}
	if err := os.Rename(tmp, filepath.Join(p.dir, snapFile)); err != nil {
		log.Printf("sodp: persist: compact rename snap: %v", err)
		return
	}

	p.mu.Lock()
	defer p.mu.Unlock()
	p.wal.Close()
	newWAL, err := os.OpenFile(filepath.Join(p.dir, walFile), os.O_TRUNC|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		log.Printf("sodp: persist: compact reopen wal: %v", err)
		return
	}
	p.wal = newWAL
	p.enc = msgpack.NewEncoder(newWAL)
	p.count = 0
}

// LoadAll reads the snapshot and replays the WAL into store.
// Must be called before the server starts accepting connections.
func (p *Persist) LoadAll(store *StateStore) error {
	// Snapshot: flat array [key0, ver0, val0, key1, ver1, val1, ...]
	snapPath := filepath.Join(p.dir, snapFile)
	if data, err := os.ReadFile(snapPath); err == nil {
		var flat []any
		if err := msgpack.Unmarshal(data, &flat); err == nil && len(flat)%3 == 0 {
			for i := 0; i+2 < len(flat); i += 3 {
				key, _ := flat[i].(string)
				version := toUint64Any(flat[i+1])
				value := normalizeAny(flat[i+2])
				if key != "" {
					store.LoadEntry(key, version, value)
				}
			}
		}
	}

	// WAL replay.
	walPath := filepath.Join(p.dir, walFile)
	f, err := os.Open(walPath)
	if err != nil {
		return nil // no WAL yet
	}
	defer f.Close()

	dec := msgpack.NewDecoder(f)
	for {
		var rec []any
		if err := dec.Decode(&rec); err != nil {
			if err == io.EOF {
				break
			}
			break // truncated tail — stop safely
		}
		if len(rec) < 3 {
			continue
		}
		op, _ := rec[0].(string)
		key, _ := rec[1].(string)
		version := toUint64Any(rec[2])
		if key == "" {
			continue
		}
		switch op {
		case "s":
			if len(rec) >= 4 {
				store.LoadEntry(key, version, normalizeAny(rec[3]))
			}
		case "d":
			store.LoadDelete(key, version)
		}
	}
	return nil
}

// Close flushes and closes the WAL.
func (p *Persist) Close() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.wal.Close()
}

// --- helpers ---

func countWAL(path string) int {
	f, err := os.Open(path)
	if err != nil {
		return 0
	}
	defer f.Close()
	dec := msgpack.NewDecoder(f)
	n := 0
	for {
		var rec []any
		if err := dec.Decode(&rec); err != nil {
			break
		}
		n++
	}
	return n
}

func toUint64Any(v any) uint64 {
	switch n := v.(type) {
	case uint64:
		return n
	case int64:
		return uint64(n)
	case float64:
		return uint64(n)
	case uint8:
		return uint64(n)
	case uint16:
		return uint64(n)
	case uint32:
		return uint64(n)
	case int8:
		return uint64(n)
	case int16:
		return uint64(n)
	case int32:
		return uint64(n)
	}
	return 0
}

// normalizeAny converts map[interface{}]interface{} (which msgpack may produce
// for non-string-key maps) into map[string]interface{} so the diff algorithm
// and JSON serialisation can handle it correctly.
func normalizeAny(v any) any {
	switch val := v.(type) {
	case map[interface{}]interface{}:
		m := make(map[string]any, len(val))
		for k, v2 := range val {
			if ks, ok := k.(string); ok {
				m[ks] = normalizeAny(v2)
			}
		}
		return m
	case map[string]interface{}:
		for k, v2 := range val {
			val[k] = normalizeAny(v2)
		}
		return val
	case []interface{}:
		for i, item := range val {
			val[i] = normalizeAny(item)
		}
		return val
	}
	return v
}
