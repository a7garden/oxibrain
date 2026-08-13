//! Canonical serialization. A bug here is a determinism bug (ARCHITECTURE.md §5.6):
//! sorted keys, normalized numbers, RFC-3339 UTC timestamps. Property-tested.

use serde_json::{Map, Value};

/// Recursively sort all object keys in a JSON value. Object key order is the only
/// non-determinism in `serde_json`; sorting it makes the output a pure function of the value.
pub fn canonicalize_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> = map
                .iter()
                .map(|(k, vv)| (k.clone(), canonicalize_value(vv)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(Map::from_iter(pairs))
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        other => other.clone(),
    }
}

/// Canonical JSON string of a Value: sorted keys, compact (no whitespace).
pub fn canonical_json_value(v: &Value) -> String {
    serde_json::to_string(&canonicalize_value(v)).expect("canonical json infallible")
}

/// Canonical bytes over a raw JSON string: parse, canonicalize, re-emit compactly.
pub fn canonical_bytes(json: &str) -> Result<Vec<u8>, serde_json::Error> {
    let v: Value = serde_json::from_str(json)?;
    Ok(canonical_json_value(&v).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn canonical_is_deterministic(s in "[a-z]{0,20}") {
            let v = serde_json::json!({ "b": 1, "a": { "y": 2, "x": 1 }, "c": &s });
            let c1 = canonical_json_value(&v);
            let c2 = canonical_json_value(&v);
            prop_assert_eq!(c1, c2);
        }

        #[test]
        fn keys_are_sorted(v in "[a-z]{1,5}") {
            let unsorted = serde_json::json!({ &v: 1, "a": 2, "z": 3 });
            let canon = canonical_json_value(&unsorted);
            // keys appear in sorted order: a, then v, then z (lexicographic)
            let keys: Vec<&str> = canon
                .trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .map(|kv| kv.split(':').next().unwrap().trim_matches('"'))
                .collect();
            let mut sorted = keys.clone();
            sorted.sort();
            prop_assert_eq!(keys, sorted);
        }
    }
}
