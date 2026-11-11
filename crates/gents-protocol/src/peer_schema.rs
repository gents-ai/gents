//! The active collection versions a runtime publishes on its `/status`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level `/status` field carrying the runtime's [`ReplicatedSchema`] for
/// the client route. `null` means the runtime could not read its own node yet.
pub const STATUS_REPLICATED_SCHEMA_FIELD: &str = "replicated_schema";

/// What a merge depends on for one collection: DefraDB's content-addressed
/// active version ID, plus the branchable and policy settings that version
/// ID does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicatedCollectionIdentity {
    pub version_id: String,
    pub branchable: bool,
    pub policy_resource: Option<String>,
}

impl ReplicatedCollectionIdentity {
    pub fn from_collection_version(version: &Value) -> Option<Self> {
        let version_id = version
            .get("VersionID")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())?
            .to_string();
        Some(Self {
            version_id,
            branchable: version
                .get("IsBranchable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            policy_resource: version
                .get("Policy")
                .filter(|policy| !policy.is_null())
                .map(|policy| {
                    policy
                        .get("ResourceName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                }),
        })
    }
}

/// Active identities by collection name, as read from one node.
pub type ReplicatedSchema = BTreeMap<String, ReplicatedCollectionIdentity>;

pub fn signed_schema_evidence_matches(signed: Option<&str>, local: Option<&str>) -> bool {
    matches!((signed, local), (Some(signed), Some(local)) if !signed.is_empty() && signed == local)
}

#[derive(Debug, thiserror::Error)]
pub enum ReplicatedSchemaDigestError {
    #[error("replicated schema is missing collection {0}")]
    MissingCollection(String),
    #[error("replicated schema selection is empty")]
    EmptySelection,
    #[error("could not encode replicated schema")]
    Encoding(#[from] serde_json::Error),
}

/// Commits to the live identity of every selected collection in sorted order.
pub fn replicated_schema_digest(
    expected: &[&str],
    schema: &ReplicatedSchema,
) -> Result<String, ReplicatedSchemaDigestError> {
    use sha2::{Digest, Sha256};
    let names: BTreeSet<_> = expected.iter().copied().collect();
    if names.is_empty() {
        return Err(ReplicatedSchemaDigestError::EmptySelection);
    }
    let mut selected = ReplicatedSchema::new();
    for name in names {
        let identity = schema
            .get(name)
            .ok_or_else(|| ReplicatedSchemaDigestError::MissingCollection(name.to_owned()))?;
        selected.insert(name.to_owned(), identity.clone());
    }
    let encoded = serde_json::to_string(&selected)?;
    let framed = crate::enrollment::canonical_domain_payload(
        "gents-live-replicated-schema-v1",
        [encoded.as_str()],
    );
    Ok(format!("sha256:{:x}", Sha256::digest(framed)))
}

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn signed_evidence_matches_the_executable_model() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["enrollment_invite_schema_cases"]
            .as_array()
            .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                signed_schema_evidence_matches(case["signed"].as_str(), case["local"].as_str()),
                case["matches"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn digest_binds_selected_versions_branchability_and_policy() {
        let identity = ReplicatedCollectionIdentity {
            version_id: "version-1".into(),
            branchable: true,
            policy_resource: Some("board".into()),
        };
        let schema = ReplicatedSchema::from([("Game".into(), identity)]);
        let original = replicated_schema_digest(&["Game"], &schema).unwrap();
        for changed in [
            ReplicatedCollectionIdentity {
                version_id: "version-2".into(),
                branchable: true,
                policy_resource: Some("board".into()),
            },
            ReplicatedCollectionIdentity {
                version_id: "version-1".into(),
                branchable: false,
                policy_resource: Some("board".into()),
            },
            ReplicatedCollectionIdentity {
                version_id: "version-1".into(),
                branchable: true,
                policy_resource: None,
            },
        ] {
            assert_ne!(
                original,
                replicated_schema_digest(
                    &["Game"],
                    &ReplicatedSchema::from([("Game".into(), changed)])
                )
                .unwrap()
            );
        }
        assert!(replicated_schema_digest(&["Other"], &schema).is_err());
        assert!(replicated_schema_digest(&[], &schema).is_err());
    }
}
