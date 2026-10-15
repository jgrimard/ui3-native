//! Lenient serde helpers. Blue Iris is inconsistent about JSON types (numbers as strings, 0/1 for booleans, etc).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub fn bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(value_to_bool(&Value::deserialize(d)?))
}

pub fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.is_null() { None } else { Some(value_to_bool(&v)) })
}

pub fn i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(value_to_f64(&Value::deserialize(d)?).map(|f| f as i64).unwrap_or(0))
}

pub fn f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(value_to_f64(&Value::deserialize(d)?).unwrap_or(0.0))
}

pub fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(value_to_f64(&Value::deserialize(d)?))
}

pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => v.to_string(),
    })
}

pub fn value_to_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        _ => false,
    }
}

pub fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// A list of strings; tolerates null, a single string, and non-string items (stringified).
pub fn string_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let item = |v: Value| match v {
        Value::String(s) => s,
        v => v.to_string(),
    };
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a.into_iter().filter(|v| !v.is_null()).map(item).collect(),
        Value::Null => Vec::new(),
        v => vec![item(v)],
    })
}
