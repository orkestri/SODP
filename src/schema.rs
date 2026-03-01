use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

// ── Schema node ───────────────────────────────────────────────────────────────

/// A schema node is either a bare type name (`"Int"`) or a full descriptor
/// object (`{ "type": "Object", "nullable": true, "fields": {...} }`).
///
/// Supported type names: `String | Int | Float | Bool | Object | Array | Any`
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SchemaNode {
    /// Compact form — just a type name string, non-nullable, no sub-fields.
    Simple(String),
    /// Full form — allows `nullable` and `fields` for Object types.
    Full {
        #[serde(rename = "type")]
        type_name: String,
        #[serde(default)]
        nullable:  bool,
        #[serde(default)]
        fields:    HashMap<String, SchemaNode>,
    },
}

impl SchemaNode {
    fn type_name(&self) -> &str {
        match self {
            SchemaNode::Simple(t)          => t,
            SchemaNode::Full { type_name, .. } => type_name,
        }
    }

    fn nullable(&self) -> bool {
        match self {
            SchemaNode::Simple(_)              => false,
            SchemaNode::Full { nullable, .. }  => *nullable,
        }
    }

    fn fields(&self) -> Option<&HashMap<String, SchemaNode>> {
        match self {
            SchemaNode::Simple(_) => None,
            SchemaNode::Full { fields, .. } => Some(fields),
        }
    }
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate `value` against `node` at JSON path `path`.
fn validate_node(value: &Value, node: &SchemaNode, path: &str) -> Result<(), String> {
    // Null is only allowed for nullable fields.
    if value.is_null() {
        return if node.nullable() {
            Ok(())
        } else {
            Err(format!("{path}: unexpected null (field is not nullable)"))
        };
    }

    match node.type_name() {
        "Any" => Ok(()),

        "String" => {
            if value.is_string() { Ok(()) }
            else { Err(format!("{path}: expected String, got {}", kind(value))) }
        }

        "Int" => {
            // Allow only integers (no fractional floats).
            if value.is_i64() || value.is_u64() { Ok(()) }
            else { Err(format!("{path}: expected Int, got {}", kind(value))) }
        }

        "Float" => {
            // Widening: integers are valid Floats.
            if value.is_number() { Ok(()) }
            else { Err(format!("{path}: expected Float, got {}", kind(value))) }
        }

        "Bool" => {
            if value.is_boolean() { Ok(()) }
            else { Err(format!("{path}: expected Bool, got {}", kind(value))) }
        }

        "Array" => {
            if value.is_array() { Ok(()) }
            else { Err(format!("{path}: expected Array, got {}", kind(value))) }
        }

        "Object" => {
            let obj = match value.as_object() {
                Some(o) => o,
                None => return Err(format!("{path}: expected Object, got {}", kind(value))),
            };

            // Recurse into declared fields only.  Extra fields in the value are
            // allowed (permissive) — the schema is a floor, not a ceiling.
            if let Some(fields) = node.fields() {
                for (field_name, field_schema) in fields {
                    let field_val = obj.get(field_name).unwrap_or(&Value::Null);
                    let field_path = format!("{path}.{field_name}");
                    validate_node(field_val, field_schema, &field_path)?;
                }
            }

            Ok(())
        }

        other => Err(format!("{path}: unknown schema type '{other}'")),
    }
}

/// Human-readable JSON value kind for error messages.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null      => "null",
        Value::Bool(_)   => "Bool",
        Value::Number(n) => if n.is_f64() { "Float" } else { "Int" },
        Value::String(_) => "String",
        Value::Array(_)  => "Array",
        Value::Object(_) => "Object",
    }
}

// ── Schema registry ───────────────────────────────────────────────────────────

/// Registry mapping state key → root `SchemaNode`.
///
/// Keys not present in the registry pass validation unchecked (permissive).
pub struct SchemaRegistry {
    states: HashMap<String, SchemaNode>,
}

impl SchemaRegistry {
    /// Load a schema from a JSON file.
    ///
    /// The file must be a JSON object whose keys are state key strings and
    /// whose values are `SchemaNode` descriptors.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let states: HashMap<String, SchemaNode> = serde_json::from_str(&raw)?;
        tracing::info!("Schema loaded from {path:?}: {} state key(s)", states.len());
        Ok(SchemaRegistry { states })
    }

    /// Validate `value` against the schema for `key`.
    ///
    /// Returns `Ok(())` when the key has no schema entry — unknown keys are
    /// always permitted (backwards-compatible with schema-less deployments).
    pub fn validate(&self, key: &str, value: &Value) -> Result<(), String> {
        match self.states.get(key) {
            None       => Ok(()),   // no schema for this key — allow any value
            Some(node) => validate_node(value, node, key),
        }
    }
}
