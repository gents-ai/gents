//! Generated-case structs for the Lean `SelfConfig` model
//! (`proofs/Proofs/SelfConfig/`): per-target field tables and patch-merge
//! witness cases consumed by `tests/conformance/self_config.rs`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigFieldTable {
    pub(crate) collection: String,
    pub(crate) unique_field: String,
    pub(crate) category: String,
    pub(crate) all_fields: Vec<String>,
    pub(crate) writable_fields: Vec<String>,
    pub(crate) protected_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigFieldValue {
    pub(crate) field: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigPatchEntry {
    pub(crate) field: String,
    /// `"set"` or `"clear"`.
    pub(crate) action: String,
    pub(crate) value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigCase {
    pub(crate) name: String,
    pub(crate) collection: String,
    #[allow(dead_code)]
    pub(crate) category: String,
    pub(crate) guarded: bool,
    pub(crate) validates: bool,
    pub(crate) doc: Vec<LeanSelfConfigFieldValue>,
    pub(crate) patch: Vec<LeanSelfConfigPatchEntry>,
    pub(crate) admissible: bool,
    pub(crate) accepted: bool,
    pub(crate) result: Vec<LeanSelfConfigFieldValue>,
    pub(crate) protected_preserved: bool,
    pub(crate) containment_holds: bool,
    pub(crate) unchanged_on_reject: bool,
    pub(crate) gate_on_after_accept: bool,
}
