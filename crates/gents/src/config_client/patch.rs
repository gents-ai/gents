//! Typed patch layer for agent self-configuration (#654).
//!
//! Mirrors the Lean `SelfConfig` model (`proofs/Proofs/SelfConfig/`): each
//! config collection has a declared writable field set; everything else —
//! identity/unique keys, the owner `agent_did`, runtime-owned status fields,
//! secrets, apply-managed fields — is protected. A patch is an in-memory
//! partial merge over exactly the writable fields ([`apply_patch`]), rejected
//! wholesale if it names anything outside them ([`ensure_admissible`]),
//! validated as a whole document, and committed through a
//! [`super::ConfigApplyTxn`] under the agent DID.
//!
//! Desired-field selection and merge semantics are fenced against the Lean contract
//! snapshot by `tests/conformance/self_config.rs`:
//! - `all_fields` reuses the canonical desired-state projection; runtime-owned
//!   observations are excluded even when they share the same stored document;
//! - identity immutability, field containment, and reject-leaves-unchanged
//!   replay the generated Lean witness cases through this merge.

use anyhow::{bail, Result};
use serde_json::{Map, Value};

/// Canonical self-configuration targets; collection metadata belongs to the shared catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfConfigTarget {
    AgentBehavior,
    AgentContext,
    Compaction,
    Tools,
    InferenceProfile,
    InferenceSampling,
    InferenceExecution,
    InferenceRetryPolicy,
    InferenceBackend,
    ToolServiceRegistry,
    Task,
    Schedule,
    Trigger,
    EventSource,
}
pub const SELF_CONFIG_CATEGORIES: [&str; 7] = [
    "behavior",
    "tools",
    "profile",
    "backend",
    "mcp_service",
    "automation",
    "persona",
];
pub const DEFAULT_SELF_CONFIG_CATEGORIES: [&str; 3] = ["behavior", "tools", "profile"];
pub const ALL_SELF_CONFIG_TARGETS: [SelfConfigTarget; 14] = [
    SelfConfigTarget::AgentBehavior,
    SelfConfigTarget::AgentContext,
    SelfConfigTarget::Compaction,
    SelfConfigTarget::Tools,
    SelfConfigTarget::InferenceProfile,
    SelfConfigTarget::InferenceSampling,
    SelfConfigTarget::InferenceExecution,
    SelfConfigTarget::InferenceRetryPolicy,
    SelfConfigTarget::InferenceBackend,
    SelfConfigTarget::ToolServiceRegistry,
    SelfConfigTarget::Task,
    SelfConfigTarget::Schedule,
    SelfConfigTarget::Trigger,
    SelfConfigTarget::EventSource,
];
impl SelfConfigTarget {
    pub fn collection(self) -> crate::Collection {
        match self {
            Self::AgentBehavior => crate::Collection::AgentBehavior,
            Self::AgentContext => crate::Collection::AgentContext,
            Self::Compaction => crate::Collection::Compaction,
            Self::Tools => crate::Collection::Tools,
            Self::InferenceProfile => crate::Collection::InferenceProfile,
            Self::InferenceSampling => crate::Collection::InferenceSampling,
            Self::InferenceExecution => crate::Collection::InferenceExecution,
            Self::InferenceRetryPolicy => crate::Collection::InferenceRetryPolicy,
            Self::InferenceBackend => crate::Collection::InferenceBackend,
            Self::ToolServiceRegistry => crate::Collection::ToolServiceRegistry,
            Self::Task => crate::Collection::Task,
            Self::Schedule => crate::Collection::Schedule,
            Self::Trigger => crate::Collection::Trigger,
            Self::EventSource => crate::Collection::EventSource,
        }
    }
    pub fn collection_name(self) -> &'static str {
        self.collection().graphql_type()
    }
    pub fn unique_field(self) -> &'static str {
        self.collection().unique_field()
    }
    pub fn from_collection_name(name: &str) -> Option<Self> {
        ALL_SELF_CONFIG_TARGETS
            .into_iter()
            .find(|target| target.collection_name() == name)
    }
    pub fn category(self) -> &'static str {
        match self {
            Self::AgentBehavior | Self::AgentContext => "behavior",
            Self::Tools => "tools",
            Self::Compaction
            | Self::InferenceProfile
            | Self::InferenceSampling
            | Self::InferenceExecution
            | Self::InferenceRetryPolicy => "profile",
            Self::InferenceBackend => "backend",
            Self::ToolServiceRegistry => "mcp_service",
            Self::Task | Self::Schedule | Self::Trigger | Self::EventSource => "automation",
        }
    }
    pub fn all_fields(self) -> &'static [&'static str] {
        super::config_projection(self.collection(), None)
            .expect("canonical config metadata")
            .0
    }
    pub fn writable_fields(self) -> Vec<&'static str> {
        self.all_fields()
            .iter()
            .copied()
            .filter(|field| {
                *field != self.unique_field()
                    && !(self == Self::Task && *field == "behavior_id")
                    && ![
                        "agent_did",
                        "created_at",
                        "updated_at",
                        "created_by",
                        "wasm_bytes",
                        "canonical_args",
                        "signer_did",
                        "provenance",
                    ]
                    .contains(field)
            })
            .collect()
    }
    pub fn protected_fields(self) -> Vec<&'static str> {
        self.all_fields()
            .iter()
            .copied()
            .filter(|field| !self.is_writable(field))
            .collect()
    }
    pub fn is_writable(self, field: &str) -> bool {
        self.writable_fields().contains(&field)
    }
}

/// A self-config patch: field → `Some(value)` to set, `None` to clear.
/// Ordered; later entries win (Lean `applyPatch` fold).
pub type SelfConfigPatch = Vec<(String, Option<Value>)>;

/// Reject unknown/protected fields before merging. Canonical typed decoding and
/// full retained reference validation check values at preview/publication.
pub fn ensure_admissible(target: SelfConfigTarget, patch: &SelfConfigPatch) -> Result<()> {
    for (field, _) in patch {
        if !target.is_writable(field) {
            if target.all_fields().contains(&field.as_str()) {
                bail!(
                    "field {field} on {collection} is protected (identity, runtime-owned, \
                     secret, or apply-managed) and cannot be patched via self-config",
                    collection = target.collection_name(),
                );
            }
            bail!(
                "unknown field {field} for {collection}; writable fields: {writable}",
                collection = target.collection_name(),
                writable = target.writable_fields().join(", "),
            );
        }
    }
    Ok(())
}

/// In-memory partial merge (Lean `applyPatch`): only writable fields change;
/// a set overwrites, a clear removes. Entries outside the writable set are
/// ignored here as defense in depth below [`ensure_admissible`].
pub fn apply_patch(
    target: SelfConfigTarget,
    doc: &Map<String, Value>,
    patch: &SelfConfigPatch,
) -> Map<String, Value> {
    let mut merged = doc.clone();
    for (field, value) in patch {
        if !target.is_writable(field) {
            continue;
        }
        match value {
            Some(value) => {
                merged.insert(field.clone(), value.clone());
            }
            None => {
                merged.remove(field);
            }
        }
    }
    merged
}

/// One field-level delta of a dry-run preview or applied patch.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FieldDelta {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

/// Field-level diff between two document projections, in schema field order.
pub fn diff_docs(
    target: SelfConfigTarget,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<FieldDelta> {
    target
        .all_fields()
        .iter()
        .filter_map(|field| {
            let from = before.get(*field).cloned().unwrap_or(Value::Null);
            let to = after.get(*field).cloned().unwrap_or(Value::Null);
            (from != to).then(|| FieldDelta {
                field: (*field).to_string(),
                from,
                to,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_partition_is_disjoint_and_complete() {
        for target in ALL_SELF_CONFIG_TARGETS {
            let all = target.all_fields();
            let writable = target.writable_fields();
            let protected = target.protected_fields();
            for field in &writable {
                assert!(
                    all.contains(field),
                    "{}: writable field {field} missing from all_fields",
                    target.collection_name()
                );
            }
            assert_eq!(
                writable.len() + protected.len(),
                all.len(),
                "{}: partition incomplete",
                target.collection_name()
            );
            assert!(
                !writable.contains(&target.unique_field()),
                "{}: unique field must be protected",
                target.collection_name()
            );
            assert!(
                !writable.contains(&"agent_did"),
                "{}: agent_did must never be writable",
                target.collection_name()
            );
        }
    }

    #[test]
    fn merge_sets_clears_and_contains() {
        let doc = json!({
            "behavior_id": "beh-1",
            "agent_did": "did:key:z6M",
            "display_name": "old",
            "description": "m-small",
        });
        let Value::Object(doc) = doc else {
            unreachable!()
        };
        let patch: SelfConfigPatch = vec![
            ("display_name".to_string(), Some(json!("new"))),
            ("description".to_string(), None),
            ("agent_did".to_string(), Some(json!("did:key:attacker"))),
        ];
        let merged = apply_patch(SelfConfigTarget::AgentBehavior, &doc, &patch);
        assert_eq!(merged.get("display_name"), Some(&json!("new")));
        assert!(!merged.contains_key("description"));
        assert_eq!(
            merged.get("agent_did"),
            Some(&json!("did:key:z6M")),
            "protected field must survive even an inadmissible entry"
        );
        assert!(ensure_admissible(SelfConfigTarget::AgentBehavior, &patch).is_err());

        let deltas = diff_docs(SelfConfigTarget::AgentBehavior, &doc, &merged);
        assert_eq!(
            deltas,
            vec![
                FieldDelta {
                    field: "display_name".into(),
                    from: json!("old"),
                    to: json!("new"),
                },
                FieldDelta {
                    field: "description".into(),
                    from: json!("m-small"),
                    to: Value::Null,
                },
            ]
        );
    }
}
