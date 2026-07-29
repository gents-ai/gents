//! Lens module helpers — always inline bytes, never temp files.

use defra_node::{LensConfig, LensModule};

use crate::registry::LensSpec;

/// Build a [`LensConfig`] from a [`LensSpec`] and pinned version pair.
///
/// Uses [`LensModule::from_bytes`] exclusively so transform IDs are stable
/// across hosts (path-based modules hash differently).
pub fn lens_config(spec: &LensSpec, source: &str, destination: &str) -> LensConfig {
    let mut module = LensModule::from_bytes(spec.wasm.to_vec());
    if let Some(args) = spec.args_json {
        if let Ok(value) = serde_json::from_str(args) {
            module = module.with_arguments(value);
        }
    }
    LensConfig::new(source, destination, module)
}
