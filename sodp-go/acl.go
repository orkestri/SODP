package sodp

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

// aclRule is one entry in the ACL JSON file.
// Pattern uses dot-separated segments: literals, {sub} (captures JWT sub),
// and a trailing * that matches one or more remaining segments.
//
// Example rule file:
//
//	[
//	  {"pattern":"private.{sub}","read":["{sub}"],"write":["{sub}"]},
//	  {"pattern":"admin.*","read":["role:admin"],"write":["role:admin"]},
//	  {"pattern":"public.*","read":["*"],"write":["role:writer"]}
//	]
type aclRule struct {
	Pattern string   `json:"pattern"`
	Read    []string `json:"read"`
	Write   []string `json:"write"`
}

// ACL is a parsed set of access-control rules.
type ACL struct {
	rules []aclRule
}

// loadACL reads and parses an ACL JSON file.
func loadACL(path string) (*ACL, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("sodp: acl: read %s: %w", path, err)
	}
	var rules []aclRule
	if err := json.Unmarshal(data, &rules); err != nil {
		return nil, fmt.Errorf("sodp: acl: parse: %w", err)
	}
	return &ACL{rules: rules}, nil
}

// Authorize returns true if the session may perform action ("read" or "write")
// on key. Rules are evaluated in order; the first matching rule is decisive.
// If no rule matches, access is allowed (permissive default).
func (a *ACL) Authorize(sess *Session, key, action string) bool {
	for _, rule := range a.rules {
		captures, ok := matchACLPattern(rule.Pattern, key)
		if !ok {
			continue
		}
		var perms []string
		if action == "write" {
			perms = rule.Write
		} else {
			perms = rule.Read
		}
		return checkPerms(perms, sess, captures)
	}
	return true // no rule matched → allow
}

// matchACLPattern matches a dot-separated pattern against key.
// Returns (captures, true) on match, (nil, false) on miss.
func matchACLPattern(pattern, key string) (map[string]string, bool) {
	pSegs := strings.Split(pattern, ".")
	kSegs := strings.Split(key, ".")
	captures := make(map[string]string, 2)

	for i, p := range pSegs {
		if p == "*" {
			return captures, i < len(kSegs) // wildcard: must consume ≥1 remaining segment
		}
		if i >= len(kSegs) {
			return nil, false
		}
		if len(p) >= 3 && p[0] == '{' && p[len(p)-1] == '}' {
			captures[p[1:len(p)-1]] = kSegs[i]
		} else if p != kSegs[i] {
			return nil, false
		}
	}
	return captures, len(pSegs) == len(kSegs)
}

// checkPerms returns true if any permission in the list grants the session access.
//
// Permission formats:
//   - "*"         — any authenticated (or anonymous) session
//   - "{sub}"     — session's JWT sub must equal the captured {sub} segment
//   - "role:X"    — session must have role X in claims["roles"] or claims["role"]
//   - "group:X"   — session must have group X in claims["groups"]
//   - "perm:X"    — session must have permission X in claims["permissions"]
//   - "claim:K:V" — claims[K] must equal or contain V
//   - literal     — session's sub must equal this string exactly
func checkPerms(perms []string, sess *Session, captures map[string]string) bool {
	for _, perm := range perms {
		switch {
		case perm == "*":
			return true
		case perm == "{sub}":
			if sub, ok := captures["sub"]; ok && sub == sess.Sub {
				return true
			}
		case strings.HasPrefix(perm, "role:"):
			role := perm[5:]
			if aclHasClaim(sess.Claims, "roles", role) || aclHasClaim(sess.Claims, "role", role) ||
				aclHasClaim(sess.Claims, "realm_access.roles", role) {
				return true
			}
		case strings.HasPrefix(perm, "group:"):
			if aclHasClaim(sess.Claims, "groups", perm[6:]) {
				return true
			}
		case strings.HasPrefix(perm, "perm:"):
			if aclHasClaim(sess.Claims, "permissions", perm[5:]) {
				return true
			}
		case strings.HasPrefix(perm, "claim:"):
			// "claim:KEY:VALUE"
			rest := perm[6:]
			if colon := strings.IndexByte(rest, ':'); colon > 0 {
				if aclHasClaim(sess.Claims, rest[:colon], rest[colon+1:]) {
					return true
				}
			}
		default:
			if perm == sess.Sub {
				return true
			}
		}
	}
	return false
}

// aclHasClaim checks whether claims[key] equals value or is a list containing value.
// Supports dot-traversal for nested claims (e.g. "realm_access.roles").
func aclHasClaim(claims map[string]any, key, value string) bool {
	var v any
	if dot := strings.IndexByte(key, '.'); dot >= 0 {
		parent, ok := claims[key[:dot]].(map[string]any)
		if !ok {
			return false
		}
		v = parent[key[dot+1:]]
	} else {
		v = claims[key]
	}
	switch cv := v.(type) {
	case string:
		return cv == value
	case []any:
		for _, item := range cv {
			if s, ok := item.(string); ok && s == value {
				return true
			}
		}
	}
	return false
}
