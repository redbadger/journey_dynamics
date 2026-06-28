//! JSON path helpers for reading and writing into deeply nested [`serde_json::Value`]
//! trees using `jsonptr` pointers.
//!
//! Pointers are RFC6901 JSON Pointers and therefore MUST start with a leading `/`.

use std::collections::BTreeMap;

use serde_json::Value;

use jsonptr::{Pointer, PointerBuf, assign::Error as AssignError};

// ── flatten ───────────────────────────────────────────────────────────────────

/// Flatten a nested JSON value into a map from [`PointerBuf`] to leaf values.
#[must_use]
pub fn flatten(source: &Value) -> BTreeMap<PointerBuf, Value> {
    let mut result = BTreeMap::new();
    flatten_into(source, &mut result, &PointerBuf::new());
    result
}

fn flatten_into(value: &Value, result: &mut BTreeMap<PointerBuf, Value>, prefix: &PointerBuf) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                let new_prefix = prefix.with_trailing_token(key);
                flatten_into(val, result, &new_prefix);
            }
        }
        Value::Array(arr) => {
            for (i, val) in arr.iter().enumerate() {
                let new_prefix = prefix.with_trailing_token(i);
                flatten_into(val, result, &new_prefix);
            }
        }
        leaf => {
            if prefix.is_empty() {
                return;
            }
            result.insert(prefix.clone(), leaf.clone());
        }
    }
}

// ── assign_all ────────────────────────────────────────────────────────────────

/// Assign a group of values, keyed by [`PointerBuf`], into an existing JSON value.
///
/// # Errors
///
/// Invalid JSON pointer errors are propagated as [`AssignError`].
///
pub fn assign_all<'a, I, P>(dest: &mut Value, changes: I) -> Result<(), AssignError>
where
    I: IntoIterator<Item = (P, &'a Value)>,
    P: AsRef<Pointer>,
{
    for (ptr, value) in changes {
        ptr.as_ref().assign(dest, value.clone())?;
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path(s: &str) -> PointerBuf {
        s.parse().unwrap()
    }

    fn rehydrate(changes: &BTreeMap<PointerBuf, Value>) -> Value {
        let mut v = json!({});
        assign_all(&mut v, changes).unwrap();

        v
    }

    // ── flatten ───────────────────────────────────────────────────────────

    #[test]
    fn flatten_nested_object() {
        let v = json!({"search": {"origin": "LHR", "destination": "JFK"}});
        let flat = flatten(&v);
        assert_eq!(flat[&path("/search/origin")], json!("LHR"));
        assert_eq!(flat[&path("/search/destination")], json!("JFK"));
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn flatten_array_of_objects() {
        let v = json!({"persons": [{"name": "Alice"}, {"name": "Bob"}]});
        let flat = flatten(&v);
        assert_eq!(flat[&path("/persons/0/name")], json!("Alice"));
        assert_eq!(flat[&path("/persons/1/name")], json!("Bob"));
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn flatten_scalar_leaf() {
        let v = json!({"count": 42});
        let flat = flatten(&v);
        assert_eq!(flat[&path("/count")], json!(42));
    }

    #[test]
    fn flatten_null_leaf_is_preserved() {
        let v = json!({"flag": null});
        let flat = flatten(&v);
        assert_eq!(flat[&path("/flag")], json!(null));
    }

    // ── assign_all ────────────────────────────────────────────────────────

    #[test]
    fn assign_all_flat_map_to_nested() {
        let mut changes = BTreeMap::new();
        changes.insert(path("/search/origin"), json!("LHR"));
        changes.insert(path("/search/destination"), json!("JFK"));

        let mut v = json!({"search": {"date": "2026-06-11"}});

        assign_all(&mut v, &changes).unwrap();

        assert_eq!(
            v,
            json!({"search": {"date": "2026-06-11", "origin": "LHR", "destination": "JFK"}})
        );
    }

    // ── round-trip ────────────────────────────────────────────────────────

    #[test]
    fn roundtrip_simple_nested_object() {
        let original = json!({"search": {"origin": "LHR", "destination": "JFK"}});
        assert_eq!(rehydrate(&flatten(&original)), original);
    }

    #[test]
    fn roundtrip_array_of_objects() {
        let original = json!({"persons": [{"name": "Alice"}, {"name": "Bob"}]});
        assert_eq!(rehydrate(&flatten(&original)), original);
    }

    #[test]
    fn roundtrip_mixed_depth() {
        let original = json!({
            "search": {"origin": "LHR"},
            "persons": [{"name": "Alice", "age": 30}],
            "count": 1
        });
        assert_eq!(rehydrate(&flatten(&original)), original);
    }

    #[test]
    fn roundtrip_scalar_values() {
        let original = json!({"a": true, "b": 42, "c": "hello", "d": null});
        assert_eq!(rehydrate(&flatten(&original)), original);
    }
}
