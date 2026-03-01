use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DeltaOpKind {
    Add,
    Update,
    Remove,
}

/// A single atomic change within a state tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaOp {
    pub op: DeltaOpKind,
    /// JSON-pointer-style path, e.g. "/posts/983/likes"
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl DeltaOp {
    pub fn add(path: impl Into<String>, value: Value) -> Self {
        DeltaOp { op: DeltaOpKind::Add, path: path.into(), value: Some(value) }
    }

    pub fn update(path: impl Into<String>, value: Value) -> Self {
        DeltaOp { op: DeltaOpKind::Update, path: path.into(), value: Some(value) }
    }

    pub fn remove(path: impl Into<String>) -> Self {
        DeltaOp { op: DeltaOpKind::Remove, path: path.into(), value: None }
    }
}

/// Compute the minimal set of `DeltaOp`s that transforms `old` into `new`.
///
/// - Recurses into nested objects to produce fine-grained field-level ops.
/// - Arrays are treated as atomic (replaced wholesale on any change).
/// - Complexity is O(changed_fields), not O(object_size).
pub fn diff(old: &Value, new: &Value) -> Vec<DeltaOp> {
    let mut ops = Vec::new();
    diff_at("/", old, new, &mut ops);
    ops
}

fn diff_at(path: &str, old: &Value, new: &Value, ops: &mut Vec<DeltaOp>) {
    match (old, new) {
        (Value::Object(old_map), Value::Object(new_map)) => {
            // Added or updated keys
            for (key, new_val) in new_map {
                let child = join_path(path, key);
                match old_map.get(key) {
                    None => ops.push(DeltaOp::add(child, new_val.clone())),
                    Some(old_val) if old_val != new_val => {
                        if old_val.is_object() && new_val.is_object() {
                            diff_at(&child, old_val, new_val, ops);
                        } else {
                            ops.push(DeltaOp::update(child, new_val.clone()));
                        }
                    }
                    _ => {} // unchanged
                }
            }
            // Removed keys
            for key in old_map.keys() {
                if !new_map.contains_key(key) {
                    ops.push(DeltaOp::remove(join_path(path, key)));
                }
            }
        }
        (old_val, new_val) if old_val != new_val => {
            ops.push(DeltaOp::update(path, new_val.clone()));
        }
        _ => {} // no change
    }
}

fn join_path(base: &str, key: &str) -> String {
    if base == "/" {
        format!("/{key}")
    } else {
        format!("{base}/{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_change_produces_empty_diff() {
        let v = json!({ "x": 1, "y": "hello" });
        assert!(diff(&v, &v).is_empty());
    }

    #[test]
    fn simple_field_update() {
        let old = json!({ "likes": 10 });
        let new = json!({ "likes": 11 });
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op, DeltaOpKind::Update);
        assert_eq!(ops[0].path, "/likes");
        assert_eq!(ops[0].value, Some(json!(11)));
    }

    #[test]
    fn field_added() {
        let old = json!({ "title": "hello" });
        let new = json!({ "title": "hello", "body": "world" });
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op, DeltaOpKind::Add);
        assert_eq!(ops[0].path, "/body");
    }

    #[test]
    fn field_removed() {
        let old = json!({ "a": 1, "b": 2 });
        let new = json!({ "a": 1 });
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op, DeltaOpKind::Remove);
        assert_eq!(ops[0].path, "/b");
    }

    #[test]
    fn nested_object_recurses() {
        let old = json!({ "post": { "likes": 10, "title": "x" } });
        let new = json!({ "post": { "likes": 11, "title": "x" } });
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "/post/likes");
        assert_eq!(ops[0].op, DeltaOpKind::Update);
    }

    #[test]
    fn multiple_changes_in_one_diff() {
        let old = json!({ "a": 1, "b": 2, "c": 3 });
        let new = json!({ "a": 99, "b": 2, "d": 4 });
        let ops = diff(&old, &new);
        // a updated, c removed, d added
        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn root_scalar_replacement() {
        let old = json!(42);
        let new = json!(43);
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "/");
        assert_eq!(ops[0].op, DeltaOpKind::Update);
    }
}
