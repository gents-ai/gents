use super::*;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Sample {
    #[serde(default, deserialize_with = "null_as_empty_string")]
    value: String,
}

#[derive(Debug, Deserialize)]
struct DefaultSample {
    #[serde(default, deserialize_with = "null_as_default")]
    value: bool,
}

#[test]
fn null_becomes_empty_string() {
    let s: Sample = serde_json::from_value(json!({ "value": null })).unwrap();
    assert_eq!(s.value, "");
}

#[test]
fn missing_becomes_empty_string() {
    let s: Sample = serde_json::from_value(json!({})).unwrap();
    assert_eq!(s.value, "");
}

#[test]
fn string_passes_through() {
    let s: Sample = serde_json::from_value(json!({ "value": "studio-1" })).unwrap();
    assert_eq!(s.value, "studio-1");
}

#[test]
fn null_becomes_default_bool() {
    let s: DefaultSample = serde_json::from_value(json!({ "value": null })).unwrap();
    assert!(!s.value);
}
