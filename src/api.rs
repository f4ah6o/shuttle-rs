//! Stable machine-facing contracts shared by the CLI and HTTP boundaries.
//!
//! Domain values intentionally remain unchanged. Boundary adapters add this
//! version marker and pagination metadata so additive domain changes do not
//! silently become protocol changes.

use serde::Serialize;
use serde_json::{json, Map, Value};

pub const SCHEMA_VERSION: &str = "shuttle.v1";

pub fn versioned<T: Serialize>(value: &T) -> serde_json::Result<Value> {
    versioned_value(serde_json::to_value(value)?)
}

pub fn versioned_value(value: Value) -> serde_json::Result<Value> {
    match value {
        Value::Object(mut object) => {
            object.insert("schema_version".to_owned(), json!(SCHEMA_VERSION));
            Ok(Value::Object(object))
        }
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
