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

pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests;
