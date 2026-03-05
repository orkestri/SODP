//! Per-key access control for SODP.
//!
//! Rules are loaded from a JSON file (env var `SODP_ACL_FILE`) at server
//! startup.  Every WATCH, RESUME, and CALL frame is checked before any state
//! is read or modified.
//!
//! # Quick start — Keycloak example
//!
//! ```json
//! {
//!   "preset": "keycloak",
//!   "rules": [
//!     { "key": "public.*",       "read": "*",            "write": "*"            },
//!     { "key": "tenant.{sub}.*", "read": "tenant:{sub}", "write": "tenant:{sub}" },
//!     { "key": "admin.*",        "read": "role:admin",   "write": "role:admin"   },
//!     { "key": "user.{sub}.*",   "read": "{sub}",        "write": "{sub}"        }
//!   ]
//! }
//! ```
//!
//! # Built-in presets
//!
//! | Preset    | IdP          | `role` path           | `group` path    | `tenant` path       |
//! |-----------|--------------|----------------------|-----------------|---------------------|
//! | keycloak  | Keycloak     | `realm_access.roles` | `groups`        | `tenant_id`         |
//! | auth0     | Auth0        | `roles`              | `groups`        | `org_id`            |
//! | okta      | Okta         | `groups`             | `groups`        | `tenant_id`         |
//! | cognito   | AWS Cognito  | `cognito:groups`     | `cognito:groups`| `custom:tenant_id`  |
//! | generic   | Any flat JWT | `roles`              | `groups`        | `tenant_id`         |
//!
//! Override any mapping with `claim_mappings`.  Auth0 custom namespace example:
//! ```json
//! {
//!   "preset": "auth0",
//!   "claim_mappings": { "role": "https://myapp.com/roles" },
//!   "rules": [ ... ]
//! }
//! ```
//! Rules never change — only the mapping does.
//!
//! # Key pattern segments (dot-separated)
//!
//! | Segment   | Meaning                                                      |
//! |-----------|--------------------------------------------------------------|
//! | `{sub}`   | Matches any single segment; captures its value               |
//! | `*`       | Suffix wildcard — matches one or more remaining segments     |
//! | _literal_ | Must match exactly                                           |
//!
//! # Permission values
//!
//! | Permission       | Meaning                                                      |
//! |------------------|--------------------------------------------------------------|
//! | `"*"`            | Any session (authenticated or not)                           |
//! | `"{sub}"`        | `session.sub` must equal the captured key segment            |
//! | `"role:X"`       | Roles claim (via mapping) contains `X`                       |
//! | `"group:X"`      | Groups claim (via mapping) contains `X`                      |
//! | `"perm:X"`       | Permissions claim (via mapping) contains `X`                 |
//! | `"tenant:{sub}"` | Tenant claim (via mapping) equals the captured key segment   |
//! | `"KEY:VALUE"`    | Any mapped claim key contains or equals `VALUE`              |
//! | _literal_        | `session.sub` must equal this exact string                   |

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;


/// Returns the default claim path mappings for a named IdP preset.
///
/// Keys are the short names used in permission strings (`role`, `group`, etc.).
/// Values are dot-notation paths into the JWT extra claims body.
fn preset_mappings(name: &str) -> Option<HashMap<String, String>> {
    let entries: &[(&str, &str)] = match name {
        "keycloak" => &[
            // Keycloak: roles live inside realm_access.roles (nested object).
            // Groups and tenant_id are top-level claims added via protocol mapper.
            ("role",   "realm_access.roles"),
            ("group",  "groups"),
            ("perm",   "permissions"),
            ("tenant", "tenant_id"),
        ],
        "auth0" => &[
            // Auth0: roles and permissions are top-level when added via an Action.
            // Org/tenant uses the standard org_id claim (Auth0 Organizations).
            // For namespaced custom claims, override `role` in claim_mappings:
            //   "claim_mappings": { "role": "https://myapp.com/roles" }
            ("role",   "roles"),
            ("group",  "groups"),
            ("perm",   "permissions"),
            ("tenant", "org_id"),
        ],
        "okta" => &[
            // Okta: groups claim carries both roles and group memberships.
            // Custom attributes are added via the profile editor and appear
            // as top-level claims.
            ("role",   "groups"),
            ("group",  "groups"),
            ("perm",   "permissions"),
            ("tenant", "tenant_id"),
        ],
        "cognito" => &[
            // AWS Cognito: pool groups appear as cognito:groups.
            // Custom attributes use the custom: prefix.
            ("role",   "cognito:groups"),
            ("group",  "cognito:groups"),
            ("perm",   "permissions"),
            ("tenant", "custom:tenant_id"),
        ],
        "generic" => &[
            // Simple flat JWT — roles, groups, permissions, tenant_id at the top level.
            // Good default for custom token issuers.
            ("role",   "roles"),
            ("group",  "groups"),
            ("perm",   "permissions"),
            ("tenant", "tenant_id"),
        ],
        _ => return None,
    };
    Some(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}


#[derive(Deserialize)]
struct AclFile {
    /// Optional built-in preset.  Loads default claim_mappings for that IdP.
    /// Supported values: `"keycloak"`, `"auth0"`, `"okta"`, `"cognito"`, `"generic"`.
    #[serde(default)]
    preset: Option<String>,

    /// Claim path mappings — overrides / extends the preset.
    /// Key: short name used in permission strings (`role`, `group`, `tenant`, …).
    /// Value: dot-notation path into the JWT body (`realm_access.roles`, …).
    #[serde(default)]
    claim_mappings: HashMap<String, String>,

    rules: Vec<AclRule>,
}

#[derive(Debug, Deserialize)]
struct AclRule {
    /// Dot-separated key pattern with optional `{sub}` capture and `*` suffix wildcard.
    key:   String,
    /// Permission required to read (WATCH / RESUME) this key.
    read:  String,
    /// Permission required to write (any CALL method) this key.
    write: String,
}


/// Loaded and compiled ACL rule set.
#[derive(Debug)]
pub struct AclRegistry {
    rules:    Vec<AclRule>,
    /// Resolved claim path mappings (preset + overrides).
    mappings: HashMap<String, String>,
}

impl AclRegistry {
    /// Load rules from a JSON file.  Merges the preset mappings (if specified)
    /// with any explicit `claim_mappings`; explicit entries win.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("ACL file '{}': {e}", path.display()))?;
        let file: AclFile = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("ACL file '{}': {e}", path.display()))?;

        // 1. Start with the preset (if any).
        let mut mappings: HashMap<String, String> = match file.preset.as_deref() {
            None => HashMap::new(),
            Some(name) => match preset_mappings(name) {
                Some(m) => m,
                None => {
                    tracing::warn!(
                        "Unknown ACL preset '{}'. \
                         Known presets: keycloak, auth0, okta, cognito, generic",
                        name
                    );
                    HashMap::new()
                }
            },
        };

        // 2. Apply explicit overrides / additions on top.
        mappings.extend(file.claim_mappings);

        Ok(AclRegistry { rules: file.rules, mappings })
    }

    /// Returns `true` if `sub` may read `key` (WATCH / RESUME).
    pub fn can_read(&self, key: &str, sub: Option<&str>, claims: &Value) -> bool {
        self.check(key, sub, claims, false)
    }

    /// Returns `true` if `sub` may write `key` (any CALL method).
    pub fn can_write(&self, key: &str, sub: Option<&str>, claims: &Value) -> bool {
        self.check(key, sub, claims, true)
    }

    fn check(&self, key: &str, sub: Option<&str>, claims: &Value, write: bool) -> bool {
        for rule in &self.rules {
            if let Some(captured) = match_pattern(&rule.key, key) {
                let perm = if write { &rule.write } else { &rule.read };
                return check_permission(perm, sub, captured.as_deref(), claims, &self.mappings);
            }
        }
        false // no matching rule → deny
    }
}


/// Match `pattern` against `key`.
///
/// Returns:
/// - `None` — does not match
/// - `Some(None)` — matches, no `{sub}` segment present
/// - `Some(Some(value))` — matches, `{sub}` captured `value`
fn match_pattern(pattern: &str, key: &str) -> Option<Option<String>> {
    let pat: Vec<&str> = pattern.split('.').collect();
    let key: Vec<&str> = key.split('.').collect();
    let mut captured: Option<String> = None;
    let mut pi = 0;
    let mut ki = 0;

    while pi < pat.len() {
        match pat[pi] {
            "*" => {
                // Suffix wildcard — must match at least one remaining segment.
                // "public.*" does not match "public" (the dot implies a sub-path).
                return if ki < key.len() { Some(captured) } else { None };
            }
            "{sub}" => {
                if ki >= key.len() { return None; }
                captured = Some(key[ki].to_string());
                pi += 1;
                ki += 1;
            }
            literal => {
                if ki >= key.len() || key[ki] != literal { return None; }
                pi += 1;
                ki += 1;
            }
        }
    }

    // Pattern exhausted — key must also be fully consumed (no trailing segments).
    if ki == key.len() { Some(captured) } else { None }
}


/// Resolve a claim path into the JWT claims value.
///
/// Two-phase strategy that handles all IdP formats:
///
/// 1. **Direct key lookup** — handles keys that contain dots or colons verbatim,
///    e.g. Auth0 namespace URLs (`"https://myapp.com/roles"`) and Cognito
///    prefixed attributes (`"cognito:groups"`).
///
/// 2. **Dot-notation traversal** — handles nested objects, e.g. Keycloak's
///    `"realm_access.roles"` → `claims["realm_access"]["roles"]`.
fn resolve_claim<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    let obj = claims.as_object()?;

    // Phase 1: try the full path as a single literal key.
    if let Some(v) = obj.get(path) {
        return Some(v);
    }

    // Phase 2: dot-separated traversal.
    let mut current = claims;
    for part in path.split('.') {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}


fn check_permission(
    perm:     &str,
    sub:      Option<&str>,
    captured: Option<&str>,
    claims:   &Value,
    mappings: &HashMap<String, String>,
) -> bool {
    match perm {
        //  Built-in shortcuts 
        "*" => true,

        // Session's sub must equal the value captured from the key pattern.
        "{sub}" => sub.is_some() && sub == captured,

        //  Claim-based: "KEY:VALUE" 
        p if p.contains(':') => {
            let (prefix, raw_value) = p.split_once(':').expect("colon guaranteed by contains check");

            // `{sub}` in the value position is a backreference to the captured segment.
            let value: &str = if raw_value == "{sub}" {
                match captured {
                    Some(c) => c,
                    None    => return false,
                }
            } else {
                raw_value
            };

            // Look up the claim path registered for this prefix.
            let claim_path = match mappings.get(prefix) {
                Some(p) => p.as_str(),
                None    => return false,
            };

            match resolve_claim(claims, claim_path) {
                // Array claim (roles, groups, permissions) — membership check.
                Some(Value::Array(arr)) => {
                    arr.iter().any(|item| item.as_str() == Some(value))
                }
                // Scalar claim (tenant, org) — equality check.
                Some(Value::String(s)) => s.as_str() == value,
                _ => false,
            }
        }

        //  Literal sub check 
        literal => sub == Some(literal),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a registry directly from a JSON string (bypasses file I/O).
    fn registry(json_str: &str) -> AclRegistry {
        let file: AclFile = serde_json::from_str(json_str).unwrap();
        let mut mappings = file.preset.as_deref()
            .and_then(preset_mappings)
            .unwrap_or_default();
        mappings.extend(file.claim_mappings);
        AclRegistry { rules: file.rules, mappings }
    }

    fn no_claims() -> Value { json!({}) }

    //  Pattern matching 

    #[test]
    fn wildcard_star_is_suffix() {
        assert!(match_pattern("public.*", "public.doc").is_some());
        assert!(match_pattern("public.*", "public.doc.nested").is_some());
        assert!(match_pattern("public.*", "public").is_none()); // no sub-segment
        assert!(match_pattern("public.*", "other.doc").is_none());
    }

    #[test]
    fn sub_placeholder_captures() {
        assert_eq!(
            match_pattern("user.{sub}.profile", "user.alice.profile"),
            Some(Some("alice".to_string()))
        );
        assert!(match_pattern("user.{sub}.profile", "user.alice.other").is_none());
    }

    #[test]
    fn exact_literal() {
        assert!(match_pattern("collab.doc", "collab.doc").is_some());
        assert!(match_pattern("collab.doc", "collab.cursors").is_none());
        assert!(match_pattern("collab.doc", "collab.doc.extra").is_none());
    }

    //  Basic ACL logic 

    #[test]
    fn open_public() {
        let acl = registry(r#"{"rules":[{"key":"public.*","read":"*","write":"*"}]}"#);
        assert!(acl.can_read("public.doc", None, &no_claims()));
        assert!(acl.can_write("public.doc", Some("alice"), &no_claims()));
        assert!(!acl.can_read("secret.doc", Some("alice"), &no_claims()));
    }

    #[test]
    fn sub_scoped_key() {
        let acl = registry(r#"{"rules":[
            {"key":"user.{sub}.*","read":"{sub}","write":"{sub}"},
            {"key":"public.*",    "read":"*",     "write":"*"    }
        ]}"#);
        assert!(acl.can_read("user.alice.notes", Some("alice"), &no_claims()));
        assert!(!acl.can_read("user.alice.notes", Some("bob"), &no_claims()));
        assert!(acl.can_read("public.board", Some("bob"), &no_claims()));
    }

    #[test]
    fn literal_sub_permission() {
        let acl = registry(r#"{"rules":[{"key":"admin.*","read":"admin","write":"admin"}]}"#);
        assert!(acl.can_read("admin.config", Some("admin"), &no_claims()));
        assert!(!acl.can_read("admin.config", Some("alice"), &no_claims()));
        assert!(!acl.can_read("admin.config", None, &no_claims()));
    }

    #[test]
    fn first_rule_wins() {
        let acl = registry(r#"{"rules":[
            {"key":"*",       "read":"*",     "write":"*"    },
            {"key":"admin.*", "read":"admin", "write":"admin"}
        ]}"#);
        // The catch-all rule fires first → admin.x is readable by anyone.
        assert!(acl.can_read("admin.secret", Some("alice"), &no_claims()));
    }

    //  Claim-based permissions 

    #[test]
    fn role_check_keycloak() {
        let acl = registry(r#"{
            "preset": "keycloak",
            "rules": [{"key":"admin.*","read":"role:admin","write":"role:admin"}]
        }"#);
        let admin_claims = json!({ "realm_access": { "roles": ["user", "admin"] } });
        let user_claims  = json!({ "realm_access": { "roles": ["user"] } });

        assert!(acl.can_read("admin.config", Some("alice"), &admin_claims));
        assert!(!acl.can_read("admin.config", Some("alice"), &user_claims));
        assert!(!acl.can_read("admin.config", Some("alice"), &no_claims()));
    }

    #[test]
    fn group_check_generic() {
        let acl = registry(r#"{
            "preset": "generic",
            "rules": [{"key":"editors.*","read":"group:editors","write":"group:editors"}]
        }"#);
        let editor = json!({ "groups": ["viewers", "editors"] });
        let viewer = json!({ "groups": ["viewers"] });

        assert!(acl.can_read("editors.doc", Some("alice"), &editor));
        assert!(!acl.can_read("editors.doc", Some("bob"), &viewer));
    }

    #[test]
    fn tenant_scoped_key() {
        let acl = registry(r#"{
            "preset": "generic",
            "rules": [{"key":"tenant.{sub}.*","read":"tenant:{sub}","write":"tenant:{sub}"}]
        }"#);
        let acme = json!({ "tenant_id": "acme" });
        let other = json!({ "tenant_id": "other-corp" });

        assert!(acl.can_read("tenant.acme.docs", Some("alice"), &acme));
        assert!(!acl.can_read("tenant.acme.docs", Some("alice"), &other));
        assert!(!acl.can_read("tenant.acme.docs", Some("alice"), &no_claims()));
    }

    #[test]
    fn auth0_namespaced_claim_override() {
        // Auth0 custom namespace — override the role mapping, rules stay the same.
        let acl = registry(r#"{
            "preset": "auth0",
            "claim_mappings": { "role": "https://myapp.com/roles" },
            "rules": [{"key":"admin.*","read":"role:admin","write":"role:admin"}]
        }"#);
        let claims = json!({ "https://myapp.com/roles": ["admin", "user"] });
        assert!(acl.can_read("admin.config", Some("alice"), &claims));
        // Standard "roles" key no longer matches (overridden).
        assert!(!acl.can_read("admin.config", Some("alice"), &json!({ "roles": ["admin"] })));
    }

    #[test]
    fn cognito_groups() {
        let acl = registry(r#"{
            "preset": "cognito",
            "rules": [{"key":"admin.*","read":"role:Admins","write":"role:Admins"}]
        }"#);
        let claims = json!({ "cognito:groups": ["Admins", "Users"] });
        assert!(acl.can_read("admin.config", Some("alice"), &claims));
    }

    #[test]
    fn permission_claim_auth0() {
        let acl = registry(r#"{
            "preset": "auth0",
            "rules": [
                {"key":"docs.*","read":"perm:read:docs","write":"perm:write:docs"}
            ]
        }"#);
        let claims = json!({ "permissions": ["read:docs", "delete:posts"] });
        assert!(acl.can_read("docs.spec", Some("alice"), &claims));
        assert!(!acl.can_write("docs.spec", Some("alice"), &claims)); // no write:docs
    }

    #[test]
    fn custom_claim_mapping() {
        // Add a wholly custom mapping key the developer defines themselves.
        let acl = registry(r#"{
            "claim_mappings": { "dept": "department" },
            "rules": [{"key":"hr.*","read":"dept:HR","write":"dept:HR"}]
        }"#);
        assert!(acl.can_read("hr.records", Some("alice"), &json!({ "department": "HR" })));
        assert!(!acl.can_read("hr.records", Some("bob"), &json!({ "department": "Engineering" })));
    }

    #[test]
    fn resolve_claim_dot_traversal() {
        let claims = json!({ "realm_access": { "roles": ["admin"] } });
        let val = resolve_claim(&claims, "realm_access.roles").unwrap();
        assert_eq!(val, &json!(["admin"]));
    }

    #[test]
    fn resolve_claim_direct_key_with_special_chars() {
        // Keys that contain dots or colons must be found by direct lookup.
        let claims = json!({
            "https://myapp.com/roles": ["admin"],
            "cognito:groups": ["Admins"]
        });
        assert!(resolve_claim(&claims, "https://myapp.com/roles").is_some());
        assert!(resolve_claim(&claims, "cognito:groups").is_some());
    }

    #[test]
    fn keycloak_full_scenario() {
        let acl = registry(r#"{
            "preset": "keycloak",
            "rules": [
                { "key": "public.*",          "read": "*",            "write": "*"            },
                { "key": "tenant.{sub}.*",    "read": "tenant:{sub}", "write": "tenant:{sub}" },
                { "key": "admin.*",           "read": "role:admin",   "write": "role:admin"   },
                { "key": "user.{sub}.*",      "read": "{sub}",        "write": "{sub}"        }
            ]
        }"#);
        let alice = json!({
            "realm_access": { "roles": ["user"] },
            "tenant_id": "acme"
        });
        let admin = json!({
            "realm_access": { "roles": ["user", "admin"] },
            "tenant_id": "acme"
        });

        // Public — open to all
        assert!(acl.can_read("public.board", Some("alice"), &alice));

        // Tenant-scoped — alice can access acme tenant, not other
        assert!(acl.can_write("tenant.acme.docs", Some("alice"), &alice));
        assert!(!acl.can_write("tenant.other.docs", Some("alice"), &alice));

        // Admin key — requires admin role
        assert!(!acl.can_read("admin.config", Some("alice"), &alice));
        assert!(acl.can_read("admin.config", Some("admin_user"), &admin));

        // User-scoped — sub must match captured segment
        assert!(acl.can_write("user.alice.notes", Some("alice"), &alice));
        assert!(!acl.can_write("user.bob.notes", Some("alice"), &alice));
    }
}
