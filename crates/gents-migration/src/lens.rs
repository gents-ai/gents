//! Lens module helpers — always inline bytes, never temp files.

use defra_node::{LensConfig, LensModule};

use crate::registry::LensSpec;

/// Build a [`LensConfig`] from a [`LensSpec`] and pinned version pair.
///
/// Uses [`LensModule::from_bytes`] exclusively so transform IDs are stable
/// across hosts (path-based modules hash differently).
pub fn lens_config(spec: &LensSpec<'_>, source: &str, destination: &str) -> LensConfig {
    let mut module = LensModule::from_bytes(spec.wasm.to_vec());
    if let Some(args) = spec.args_json {
        if let Ok(value) = serde_json::from_str(args) {
            module = module.with_arguments(value);
        }
    }
    LensConfig::new(source, destination, module)
}

/// Predict the transform id DefraDB will assign for a bytes lens module.
///
/// Matches `WasmTransformStore::add` / `MemoryTransformStore::add`: sha256 of
/// the serialized `Lenses` array, formatted as `baf` + 16 hex bytes.
pub fn predict_transform_id(spec: &LensSpec<'_>) -> String {
    use sha2::{Digest, Sha256};
    let module = {
        let mut m = LensModule::from_bytes(spec.wasm.to_vec());
        if let Some(args) = spec.args_json {
            if let Ok(value) = serde_json::from_str(args) {
                m = m.with_arguments(value);
            }
        }
        m
    };
    let lenses = vec![module];
    let lenses_json = serde_json::to_vec(&lenses).expect("serialize lenses");
    let mut hasher = Sha256::new();
    hasher.update(&lenses_json);
    let hash = hasher.finalize();
    format!("baf{}", hex::encode(&hash[..16]))
}
