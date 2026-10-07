//! Generated-case structs for the Lean `SelfConfig` model
//! (`proofs/Proofs/SelfConfig/`): per-target field tables and patch-merge
//! witness cases consumed by `tests/conformance/self_config.rs`. The tables
//! describe the canonical configuration collections (`ConfigDocuments`), with
//! the nested `Tools.self_config` group carried as one field value.

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

/// A backend a profile row can select; `backend_id` and `auth` are canonical
/// JSON text.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigBackend {
    pub(crate) backend_id: String,
    pub(crate) provider_kind: String,
    pub(crate) auth: String,
}

/// Operator grants the invoking agent holds (`SelfConfig.Grants`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigGrants {
    pub(crate) pack_install: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigCase {
    pub(crate) name: String,
    pub(crate) collection: String,
    pub(crate) category: String,
    pub(crate) guarded: bool,
    pub(crate) validates: bool,
    pub(crate) held_grants: LeanSelfConfigGrants,
    pub(crate) doc: Vec<LeanSelfConfigFieldValue>,
    pub(crate) patch: Vec<LeanSelfConfigPatchEntry>,
    pub(crate) admissible: bool,
    pub(crate) accepted: bool,
    pub(crate) result: Vec<LeanSelfConfigFieldValue>,
    pub(crate) protected_preserved: bool,
    pub(crate) containment_holds: bool,
    pub(crate) unchanged_on_reject: bool,
    pub(crate) control_kept_after_accept: bool,
    #[serde(default)]
    pub(crate) backends: Vec<LeanSelfConfigBackend>,
}
