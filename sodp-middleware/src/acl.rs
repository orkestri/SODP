//! Per-key access control.
//!
//! Rules are loaded from a JSON file.  If no ACL is configured, every
//! authenticated operation is allowed (open policy).
//!
//! # Rule file format
//!
//! ```json
//! {
//!   "rules": [
//!     { "key": "public.*",     "read": "*",     "write": "*"     },
//!     { "key": "admin.*",      "read": "admin", "write": "admin" },
//!     { "key": "user.{sub}.*", "read": "{sub}", "write": "{sub}" }
//!   ]
//! }
//! ```
//!
//! ## Key pattern syntax
//! - `*` — matches any sequence of characters
//! - `{sub}` — replaced by the caller's authenticated `sub` before matching
//!
//! ## Permission values
//! - `"*"` — any authenticated user
//! - `"{sub}"` — only the user whose `sub` matches the `{sub}` segment in the key
//! - Any other string — only users whose `sub` equals that string exactly
//!
//! First matching rule wins.  Keys with no matching rule are **denied** when an
//! ACL is configured, or **allowed** when no ACL is configured.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

// ── Deserialization types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RuleFile {
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    key:   String,
    read:  String,
    write: String,
}

// ── Internal representation ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Permission {
    /// Any authenticated user (rule value `"*"`).
    Anyone,
    /// Only the user whose `sub` matches the `{sub}` captured in the key pattern.
    MatchSub,
    /// Only a specific subject.
    Specific(String),
}

impl Permission {
    fn from_str(s: &str) -> Self {
        match s {
            "*"     => Permission::Anyone,
            "{sub}" => Permission::MatchSub,
            other   => Permission::Specific(other.to_string()),
        }
    }

    fn allows(&self, caller_sub: Option<&str>, key_sub: Option<&str>) -> bool {
        let Some(caller) = caller_sub else { return false };
        match self {
            Permission::Anyone          => true,
            Permission::MatchSub        => Some(caller) == key_sub,
            Permission::Specific(fixed) => caller == fixed,
        }
    }
}

#[derive(Debug, Clone)]
struct AclRule {
    key_pattern: String,
    read:        Permission,
    write:       Permission,
}

// ── AclRegistry ──────────────────────────────────────────────────────────────

/// Registry of access-control rules.
///
/// Wrap in `Arc` for cheap sharing across connection tasks:
/// ```ignore
/// let acl = AclRegistry::from_file(path)?; // already Arc<Self>
/// if acl.can_write("user.alice.score", Some("alice")) { ... }
/// ```
pub struct AclRegistry {
    rules: Vec<AclRule>,
}

impl AclRegistry {
    /// Load rules from a JSON file.
    pub fn from_file(path: &Path) -> anyhow::Result<Arc<Self>> {
        let raw  = std::fs::read_to_string(path)?;
        let file: RuleFile = serde_json::from_str(&raw)?;
        let rules = file.rules.into_iter().map(|r| AclRule {
            key_pattern: r.key,
            read:        Permission::from_str(&r.read),
            write:       Permission::from_str(&r.write),
        }).collect();
        Ok(Arc::new(AclRegistry { rules }))
    }

    /// Returns `true` when `sub` is allowed to **read** `key`.
    pub fn can_read(&self, key: &str, sub: Option<&str>) -> bool {
        self.check(key, sub, |r| &r.read)
    }

    /// Returns `true` when `sub` is allowed to **write** `key`.
    pub fn can_write(&self, key: &str, sub: Option<&str>) -> bool {
        self.check(key, sub, |r| &r.write)
    }

    fn check(&self, key: &str, sub: Option<&str>, perm: impl Fn(&AclRule) -> &Permission) -> bool {
        for rule in &self.rules {
            if let Some(key_sub) = pattern_match(key, &rule.key_pattern, sub) {
                return perm(rule).allows(sub, key_sub.as_deref());
            }
        }
        false
    }
}

// ── Pattern matching ──────────────────────────────────────────────────────────

fn pattern_match(key: &str, pattern: &str, sub: Option<&str>) -> Option<Option<String>> {
    let expanded = if pattern.contains("{sub}") {
        pattern.replace("{sub}", sub.unwrap_or(""))
    } else {
        pattern.to_string()
    };
    let captured = if pattern.contains("{sub}") { sub.map(str::to_string) } else { None };
    if glob_match(key, &expanded) { Some(captured) } else { None }
}

fn glob_match(s: &str, pattern: &str) -> bool {
    let mut s_chars = s.chars().peekable();
    let mut p_chars = pattern.chars().peekable();
    while let Some(&pc) = p_chars.peek() {
        if pc == '*' {
            p_chars.next();
            if p_chars.peek().is_none() { return true; }
            let rest_pattern: String = p_chars.collect();
            let rest_s: String = s_chars.collect();
            for i in 0..=rest_s.len() {
                if glob_match(&rest_s[i..], &rest_pattern) { return true; }
            }
            return false;
        } else {
            match s_chars.next() {
                Some(sc) if sc == pc => { p_chars.next(); }
                _ => return false,
            }
        }
    }
    s_chars.peek().is_none()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact() {
        assert!(glob_match("foo.bar", "foo.bar"));
        assert!(!glob_match("foo.baz", "foo.bar"));
    }

    #[test]
    fn glob_trailing_star() {
        assert!(glob_match("user.alice.score", "user.*"));
        assert!(!glob_match("admin.secret", "user.*"));
    }

    #[test]
    fn sub_substitution() {
        let r = pattern_match("user.alice.score", "user.{sub}.*", Some("alice"));
        assert!(r.is_some());
        let r2 = pattern_match("user.alice.score", "user.{sub}.*", Some("bob"));
        assert!(r2.is_none());
    }
}
