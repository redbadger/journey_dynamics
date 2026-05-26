//! JSON path helpers for reading and writing into deeply nested [`serde_json::Value`]
//! trees using [`AttributePath`] keys.
//!
//! # Array vs. object indexing rule
//!
//! A segment that parses as a [`u32`] is treated as an **array index**; any
//! other segment is treated as an **object key**. This is a purely syntactic
//! rule: `"persons/0/name"` addresses `persons[0].name` (array), while
//! `"persons/passenger_0/name"` addresses `persons.passenger_0.name` (object).
//!
//! When writing with [`set_at_path`], intermediate nodes that do not yet exist
//! are created with the type implied by how they will be indexed: if the
//! following segment is numeric an array is created; otherwise an object. If
//! an intermediate node already exists with an incompatible type (e.g. a
//! scalar sits where an object is needed) it is **replaced**.
//!
//! # Flatten / rehydrate round-trip
//!
//! <code>[rehydrate]([flatten](x)) == x</code> holds for all values whose
//! leaves are scalars and whose container nodes (objects/arrays) are
//! non-empty. Empty objects (`{}`) and empty arrays (`[]`) are invisible to
//! `flatten` — they produce no leaf paths — and are therefore lost in a
//! round-trip.

use std::collections::BTreeMap;

use serde_json::Value;

use super::AttributePath;

// ── set_at_path ───────────────────────────────────────────────────────────────

/// Write `value` at `path` inside `target`, creating intermediate nodes as
/// needed.
///
/// See the [module-level documentation](self) for the array/object indexing
/// rule and type-replacement behaviour.
pub fn set_at_path(target: &mut Value, path: &AttributePath, value: Value) {
    let segs: Vec<&str> = path.segments().collect();
    set_at_segments(target, &segs, value);
}

/// Recursive worker for [`set_at_path`].
fn set_at_segments(target: &mut Value, segments: &[&str], value: Value) {
    match segments {
        // Reached the end of the path: replace this node.
        [] => *target = value,

        // Final segment: write the value into the current node.
        [last] => {
            if let Ok(i) = last.parse::<u32>() {
                let i = i as usize;
                coerce_to_array(target);
                let arr = target.as_array_mut().expect("coerced to array");
                if arr.len() <= i {
                    arr.resize(i + 1, Value::Null);
                }
                arr[i] = value;
            } else {
                coerce_to_object(target);
                target
                    .as_object_mut()
                    .expect("coerced to object")
                    .insert((*last).to_owned(), value);
            }
        }

        // Intermediate segment: navigate (or create) the child, then recurse.
        [head, rest @ ..] => {
            if let Ok(i) = head.parse::<u32>() {
                let i = i as usize;
                coerce_to_array(target);
                let arr = target.as_array_mut().expect("coerced to array");
                if arr.len() <= i {
                    arr.resize(i + 1, Value::Null);
                }
                set_at_segments(&mut arr[i], rest, value);
            } else {
                coerce_to_object(target);
                let child = target
                    .as_object_mut()
                    .expect("coerced to object")
                    .entry((*head).to_owned())
                    .or_insert(Value::Null);
                set_at_segments(child, rest, value);
            }
        }
    }
}

/// Replace `v` with an empty array if it is not already an array.
#[inline]
fn coerce_to_array(v: &mut Value) {
    if !v.is_array() {
        *v = Value::Array(Vec::new());
    }
}

/// Replace `v` with an empty object if it is not already an object.
#[inline]
fn coerce_to_object(v: &mut Value) {
    if !v.is_object() {
        *v = Value::Object(serde_json::Map::new());
    }
}

// ── get_at_path ───────────────────────────────────────────────────────────────

/// Return a reference to the value at `path` inside `source`, or `None` if
/// any segment along the way is missing or type-incompatible.
#[must_use]
pub fn get_at_path<'a>(source: &'a Value, path: &AttributePath) -> Option<&'a Value> {
    let mut current = source;
    for seg in path.segments() {
        if let Ok(i) = seg.parse::<u32>() {
            current = current.as_array()?.get(i as usize)?;
        } else {
            current = current.as_object()?.get(seg)?;
        }
    }
    Some(current)
}

// ── flatten ───────────────────────────────────────────────────────────────────

/// Flatten a nested JSON value into a map from [`AttributePath`] to leaf
/// values.
///
/// Objects and arrays are recursed; scalars (including `null`) at the leaves
/// are collected. Empty objects and empty arrays produce no entries.
#[must_use]
pub fn flatten(source: &Value) -> BTreeMap<AttributePath, Value> {
    let mut result = BTreeMap::new();
    flatten_into(source, &mut result, "");
    result
}

fn flatten_into(value: &Value, result: &mut BTreeMap<AttributePath, Value>, prefix: &str) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                let new_prefix = build_prefix(prefix, key);
                flatten_into(val, result, &new_prefix);
            }
        }
        Value::Array(arr) => {
            for (i, val) in arr.iter().enumerate() {
                let new_prefix = build_prefix(prefix, &i.to_string());
                flatten_into(val, result, &new_prefix);
            }
        }
        leaf => {
            if prefix.is_empty() {
                return;
            }
            // The prefix is built exclusively from map keys and array indices
            // joined by "/", so it is always a valid AttributePath. The
            // fallible parse is a safety net for degenerate JSON keys (e.g.
            // empty-string keys) that would violate the invariant.
            if let Ok(path) = prefix.parse::<AttributePath>() {
                result.insert(path, leaf.clone());
            }
        }
    }
}

/// Build a new prefix by appending `segment` to `parent` with a "/" separator.
#[inline]
fn build_prefix(parent: &str, segment: &str) -> String {
    if parent.is_empty() {
        segment.to_owned()
    } else {
        format!("{parent}/{segment}")
    }
}

// ── rehydrate ─────────────────────────────────────────────────────────────────

/// Reconstruct a nested JSON value from a flat map of path → leaf value.
///
/// This is the inverse of [`flatten`]; see the module-level documentation for
/// the round-trip invariant.
#[must_use]
pub fn rehydrate(changes: &BTreeMap<AttributePath, Value>) -> Value {
    let mut root = Value::Object(serde_json::Map::new());
    for (path, value) in changes {
        set_at_path(&mut root, path, value.clone());
    }
    root
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn path(s: &str) -> AttributePath {
        s.parse().unwrap()
    }

    // ── set_at_path ───────────────────────────────────────────────────────

    #[test]
    fn set_creates_nested_object() {
        let mut v = json!({});
        set_at_path(&mut v, &path("search/origin"), json!("LHR"));
        assert_eq!(v, json!({"search": {"origin": "LHR"}}));
    }

    #[test]
    fn set_creates_array_for_numeric_segment() {
        let mut v = json!({});
        set_at_path(&mut v, &path("persons/0/name"), json!("Alice"));
        assert_eq!(v, json!({"persons": [{"name": "Alice"}]}));
    }

    #[test]
    fn set_extends_existing_array() {
        let mut v = json!({"persons": [{"name": "Alice"}]});
        set_at_path(&mut v, &path("persons/1/name"), json!("Bob"));
        assert_eq!(v, json!({"persons": [{"name": "Alice"}, {"name": "Bob"}]}));
    }

    #[test]
    fn set_overwrites_existing_value() {
        let mut v = json!({"search": {"origin": "LHR"}});
        set_at_path(&mut v, &path("search/origin"), json!("CDG"));
        assert_eq!(v, json!({"search": {"origin": "CDG"}}));
    }

    #[test]
    fn set_replaces_scalar_with_object_for_deeper_path() {
        let mut v = json!({"a": "old_scalar"});
        set_at_path(&mut v, &path("a/b"), json!(42));
        assert_eq!(v, json!({"a": {"b": 42}}));
    }

    #[test]
    fn set_single_segment() {
        let mut v = json!({});
        set_at_path(&mut v, &path("key"), json!("value"));
        assert_eq!(v, json!({"key": "value"}));
    }

    // ── get_at_path ───────────────────────────────────────────────────────

    #[test]
    fn get_returns_nested_value() {
        let v = json!({"search": {"origin": "LHR"}});
        assert_eq!(get_at_path(&v, &path("search/origin")), Some(&json!("LHR")));
    }

    #[test]
    fn get_returns_array_element() {
        let v = json!({"persons": [{"name": "Alice"}, {"name": "Bob"}]});
        assert_eq!(
            get_at_path(&v, &path("persons/1/name")),
            Some(&json!("Bob"))
        );
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let v = json!({"a": 1});
        assert_eq!(get_at_path(&v, &path("b")), None);
    }

    #[test]
    fn get_returns_none_for_out_of_bounds_index() {
        let v = json!({"arr": [1, 2]});
        assert_eq!(get_at_path(&v, &path("arr/5")), None);
    }

    // ── flatten ───────────────────────────────────────────────────────────

    #[test]
    fn flatten_nested_object() {
        let v = json!({"search": {"origin": "LHR", "destination": "JFK"}});
        let flat = flatten(&v);
        assert_eq!(flat[&path("search/origin")], json!("LHR"));
        assert_eq!(flat[&path("search/destination")], json!("JFK"));
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn flatten_array_of_objects() {
        let v = json!({"persons": [{"name": "Alice"}, {"name": "Bob"}]});
        let flat = flatten(&v);
        assert_eq!(flat[&path("persons/0/name")], json!("Alice"));
        assert_eq!(flat[&path("persons/1/name")], json!("Bob"));
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn flatten_scalar_leaf() {
        let v = json!({"count": 42});
        let flat = flatten(&v);
        assert_eq!(flat[&path("count")], json!(42));
    }

    #[test]
    fn flatten_null_leaf_is_preserved() {
        let v = json!({"flag": null});
        let flat = flatten(&v);
        assert_eq!(flat[&path("flag")], json!(null));
    }

    // ── rehydrate ─────────────────────────────────────────────────────────

    #[test]
    fn rehydrate_flat_map_to_nested() {
        let mut changes = BTreeMap::new();
        changes.insert(path("search/origin"), json!("LHR"));
        changes.insert(path("search/destination"), json!("JFK"));
        let v = rehydrate(&changes);
        assert_eq!(
            v,
            json!({"search": {"origin": "LHR", "destination": "JFK"}})
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
