//! Stable machine-facing contracts shared by the CLI and HTTP boundaries.
//!
//! Domain values intentionally remain unchanged. Boundary adapters add this
//! version marker and pagination metadata so additive domain changes do not
//! silently become protocol changes.

use serde::Serialize;
use serde_json::{json, value::RawValue, Map, Value};

pub const SCHEMA_VERSION: &str = "shuttle.v1";

/// Serialize a versioned machine-facing envelope as compact raw JSON.
///
/// Returning `RawValue` preserves single-line JSON even when a caller uses a
/// pretty serializer. This is required by streaming commands that emit NDJSON.
pub fn versioned<T: Serialize>(value: &T) -> serde_json::Result<Box<RawValue>> {
    let envelope = versioned_value(serde_json::to_value(value)?)?;
    RawValue::from_string(serde_json::to_string(&envelope)?)
}

pub fn versioned_value(value: Value) -> serde_json::Result<Value> {
    match value {
        Value::Object(mut object) if !object.contains_key("schema_version") => {
            object.insert("schema_version".to_owned(), json!(SCHEMA_VERSION));
            Ok(Value::Object(object))
        }
        Value::Object(object) => Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "value": object
        })),
        Value::Array(items) => Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "items": items,
            "pagination": {
                "returned": object_len(&items),
                "has_more": false
            }
        })),
        value => Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "value": value
        })),
    }
}

fn object_len(value: &[Value]) -> usize {
    value.len()
}

pub fn list<T: Serialize>(items: &[T], limit: Option<u32>) -> serde_json::Result<Value> {
    let items = serde_json::to_value(items)?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "items": items,
        "pagination": {
            "limit": limit,
            "returned": items.as_array().map_or(0, Vec::len),
            "has_more": limit.is_some_and(|limit| items.as_array().is_some_and(|items| items.len() >= limit as usize))
        }
    }))
}

pub fn error(code: &str, message: &str, retryable: bool) -> Value {
    let mut details = Map::new();
    details.insert("schema_version".to_owned(), json!(SCHEMA_VERSION));
    details.insert(
        "error".to_owned(),
        json!({
            "code": code,
            "message": message,
            "retryable": retryable
        }),
    );
    Value::Object(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_object_preserves_domain_schema_version() {
        let domain = json!({
            "schema_version": 7,
            "healthy": true
        });
        let envelope = versioned_value(domain.clone()).unwrap();

        assert_eq!(envelope["schema_version"], SCHEMA_VERSION);
        assert_eq!(envelope["value"], domain);
    }

    #[test]
    fn versioned_json_remains_single_line_with_pretty_serializer() {
        let envelope = versioned(&json!({"id": "event-1"})).unwrap();
        let serialized = serde_json::to_string_pretty(&envelope).unwrap();

        assert!(!serialized.contains('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(&serialized).unwrap(),
            json!({
                "schema_version": SCHEMA_VERSION,
                "id": "event-1"
            })
        );
    }
}
