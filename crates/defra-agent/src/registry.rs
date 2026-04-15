//! Shared helpers for parsing `ToolServiceRegistry` rows.
//!
//! The registry schema allows nullable address fields (`hostname`,
//! `tailscale_ip`, `lan_ip`, `mcp_path`). Default serde behavior rejects
//! explicit JSON `null` when deserializing into `String`, so consumers
//! that model these fields as `String` need a null-tolerant deserializer.

use serde::{Deserialize, Deserializer};

pub(crate) fn null_as_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct Sample {
        #[serde(default, deserialize_with = "null_as_empty_string")]
        value: String,
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
}
