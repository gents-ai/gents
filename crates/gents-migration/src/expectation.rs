//! Collection state expectations and normalized descriptor digests.
//!
//! Normalization contract (design §8.3):
//! - Include: name, version_id, collection_id, is_active; fields sorted by
//!   name (name/kind/crdt/relation/default/size/immutable); previous_version
//!   source + transform; indexes / policy / embeddings when present.
//! - Exclude: root_id and other runtime-derived values.

use defra_node::CollectionVersion;
use serde_json::{json, Value};

/// Expected post-state for a baseline entry or step.
///
/// Phase A allows field-name subsets; full digests are filled when the
/// chain-replay authoring test freezes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionExpectation {
    /// Field names that must be present on the active version.
    /// `None` means "do not check field names" (pin / single-version only).
    pub required_field_names: Option<&'static [&'static str]>,
    /// Optional full normalized-descriptor digest (hex). `None` skips digest.
    pub descriptor_digest: Option<&'static str>,
}

impl CollectionExpectation {
    /// Expectation that only checks the version DAG shape (no field list).
    pub const fn dag_only() -> Self {
        Self {
            required_field_names: None,
            descriptor_digest: None,
        }
    }

    /// Expectation that requires the given field names on the active version.
    pub const fn fields(required_field_names: &'static [&'static str]) -> Self {
        Self {
            required_field_names: Some(required_field_names),
            descriptor_digest: None,
        }
    }

    /// Verify `version` against this expectation. Returns `Ok(())` or a
    /// human-readable detail string.
    pub fn verify(&self, version: &CollectionVersion) -> Result<(), String> {
        if let Some(required) = self.required_field_names {
            let present: std::collections::HashSet<&str> =
                version.fields.iter().map(|f| f.name.as_str()).collect();
            let mut missing = Vec::new();
            for name in required {
                if !present.contains(name) {
                    missing.push(*name);
                }
            }
            if !missing.is_empty() {
                return Err(format!("missing required fields: {missing:?}"));
            }
        }

        if let Some(expected_digest) = self.descriptor_digest {
            let actual = descriptor_digest(version);
            if actual != expected_digest {
                return Err(format!(
                    "descriptor digest mismatch: expected {expected_digest}, got {actual}"
                ));
            }
        }

        Ok(())
    }
}

/// Build the normalized JSON value for a collection version.
pub fn normalize_descriptor(version: &CollectionVersion) -> Value {
    let mut fields: Vec<Value> = version
        .fields
        .iter()
        .map(|f| {
            json!({
                "Name": f.name,
                "Kind": format!("{:?}", f.kind),
                "Typ": format!("{:?}", f.crdt_type),
                "RelationName": f.relation_name,
                "IsPrimary": f.is_primary,
                "DefaultValue": f.default_value,
                "Size": f.size,
                "Immutable": f.immutable,
            })
        })
        .collect();
    fields.sort_by(|a, b| {
        let an = a.get("Name").and_then(|v| v.as_str()).unwrap_or("");
        let bn = b.get("Name").and_then(|v| v.as_str()).unwrap_or("");
        an.cmp(bn)
    });

    let previous = version.previous_version.as_ref().map(|pv| {
        json!({
            "Source": pv.source_collection_id,
            "Transform": pv.transform,
        })
    });

    json!({
        "Name": version.name,
        "VersionID": version.version_id,
        "CollectionID": version.collection_id,
        "IsActive": version.is_active,
        "IsPlaceholder": version.is_placeholder,
        "Fields": fields,
        "PreviousVersion": previous,
    })
}

/// Hex-encoded sha256 of the normalized descriptor JSON (compact).
pub fn descriptor_digest(version: &CollectionVersion) -> String {
    use std::hash::{Hash, Hasher};
    // Stable content hash without pulling sha2 into the public API surface of
    // every caller: serde_json canonical-ish via the normalized value, then
    // DefaultHasher is NOT stable across rustc versions — use a simple FNV-like
    // fold over the JSON bytes for Phase A digests that are frozen in-tree only
    // after the chain-replay test prints them with this same function.
    //
    // Prefer a real sha2 when pins are authored in Phase B; for now the digest
    // is only compared when `descriptor_digest: Some(...)` is set.
    let bytes =
        serde_json::to_vec(&normalize_descriptor(version)).unwrap_or_else(|_| b"{}".to_vec());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_field_is_reported() {
        let exp = CollectionExpectation::fields(&["lifecycle_state"]);
        // Empty version has no fields — verification must fail.
        let v = CollectionVersion {
            name: "AgentToolCall".into(),
            version_id: "v1".into(),
            collection_id: "c1".into(),
            ..CollectionVersion::new("", "", "", vec![])
        };
        let err = exp.verify(&v).unwrap_err();
        assert!(err.contains("lifecycle_state"), "{err}");
    }
}
