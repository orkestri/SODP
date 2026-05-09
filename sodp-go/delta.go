package sodp

import (
	"fmt"
	"strings"
)

// DeltaOpType identifies the kind of structural change.
type DeltaOpType string

const (
	OpAdd    DeltaOpType = "ADD"
	OpUpdate DeltaOpType = "UPDATE"
	OpRemove DeltaOpType = "REMOVE"
)

// DeltaOp represents a single field-level change within a state entry.
// Path is a JSON Pointer (RFC 6901): "/field", "/nested/field", "/-" for array append.
type DeltaOp struct {
	Op    DeltaOpType `msgpack:"op"`
	Path  string      `msgpack:"path"`
	Value any         `msgpack:"value,omitempty"`
}

// DeltaEntry is a versioned set of operations for a single key.
// It is stored in the per-key delta log and broadcast to all watchers.
type DeltaEntry struct {
	Key     string    `msgpack:"key"`
	Version uint64    `msgpack:"version"`
	Ops     []DeltaOp `msgpack:"ops"`
}

// Diff computes the field-level delta between two map values.
//
// Both old and new must be map[string]any for field-level diffing.
// If either side is not a map (including nil), a single UPDATE at "/" replaces
// the entire value. Nested maps are recursed into. Arrays are treated atomically.
// The result is O(changed_fields).
func Diff(old, new any) []DeltaOp {
	oldMap, oldOK := toStringMap(old)
	newMap, newOK := toStringMap(new)

	if !oldOK || !newOK {
		return []DeltaOp{{Op: OpUpdate, Path: "/", Value: new}}
	}

	var ops []DeltaOp

	for k, oldVal := range oldMap {
		newVal, exists := newMap[k]
		if !exists {
			ops = append(ops, DeltaOp{Op: OpRemove, Path: "/" + k})
			continue
		}
		oldNested, oldNestOK := toStringMap(oldVal)
		newNested, newNestOK := toStringMap(newVal)
		if oldNestOK && newNestOK {
			for _, sub := range Diff(oldNested, newNested) {
				sub.Path = "/" + k + sub.Path
				ops = append(ops, sub)
			}
			continue
		}
		if !equal(oldVal, newVal) {
			ops = append(ops, DeltaOp{Op: OpUpdate, Path: "/" + k, Value: newVal})
		}
	}

	for k, newVal := range newMap {
		if _, exists := oldMap[k]; !exists {
			ops = append(ops, DeltaOp{Op: OpAdd, Path: "/" + k, Value: newVal})
		}
	}

	return ops
}

// ApplyOps reconstructs a new value by applying delta operations to current.
// Used by the cluster backend to update local state from a remote node's delta
// without re-running the full diff algorithm.
func ApplyOps(current any, ops []DeltaOp) any {
	for _, op := range ops {
		current = applyOp(current, op.Op, op.Path, op.Value)
	}
	return current
}

func applyOp(current any, opType DeltaOpType, path string, value any) any {
	if path == "/" {
		if opType == OpRemove {
			return nil
		}
		return value
	}
	if len(path) < 2 || path[0] != '/' {
		return current
	}
	rest := path[1:]

	// Array operations: "/-" (append) or "/N" (index-based remove).
	if slice, ok := current.([]any); ok {
		switch {
		case rest == "-" && opType == OpAdd:
			out := make([]any, len(slice)+1)
			copy(out, slice)
			out[len(slice)] = value
			return out
		case opType == OpRemove && len(slice) > 0:
			// "/0" → remove head; any other index falls here too.
			if !strings.ContainsRune(rest, '/') {
				return slice[1:]
			}
		}
		return current
	}

	// Map operations.
	slash := strings.IndexByte(rest, '/')
	var field string
	m, _ := toStringMap(current)
	result := make(map[string]any, len(m)+1)
	for k, v := range m {
		result[k] = v
	}

	if slash == -1 {
		field = rest
		switch opType {
		case OpAdd, OpUpdate:
			result[field] = value
		case OpRemove:
			delete(result, field)
		}
	} else {
		field = rest[:slash]
		result[field] = applyOp(result[field], opType, rest[slash:], value)
	}
	return result
}

func toStringMap(v any) (map[string]any, bool) {
	if v == nil {
		return nil, false
	}
	m, ok := v.(map[string]any)
	return m, ok
}

func equal(a, b any) bool {
	switch av := a.(type) {
	case string:
		bv, ok := b.(string)
		return ok && av == bv
	case float64:
		bv, ok := b.(float64)
		return ok && av == bv
	case int64:
		bv, ok := b.(int64)
		return ok && av == bv
	case uint64:
		bv, ok := b.(uint64)
		return ok && av == bv
	case bool:
		bv, ok := b.(bool)
		return ok && av == bv
	case nil:
		return b == nil
	}
	return fmt.Sprintf("%v", a) == fmt.Sprintf("%v", b)
}
