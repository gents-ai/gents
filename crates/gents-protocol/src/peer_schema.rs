//! Live compatibility of the collection versions two running nodes replicate.
//! DefraDB resolves every merged block by `schema_version_id` and rejects
//! versions the receiver does not hold; a peer whose active versions differ can
//! never merge the other side's writes and must be reported instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level `/status` field carrying the runtime's [`ReplicatedSchema`] for
/// the client route. `null` means the runtime could not read its own node yet.
pub const STATUS_REPLICATED_SCHEMA_FIELD: &str = "replicated_schema";

/// Prevent app routes from bypassing protocol schema compatibility checks.
pub fn admit_app_collections(requested: BTreeSet<String>) -> Option<BTreeSet<String>> {
    let overlaps_protocol_catalog = requested.iter().any(|requested| {
        crate::schemas::ALL_COLLECTION_NAMES
            .iter()
            .chain(crate::schemas::RUNTIME_COLLECTION_NAMES.iter())
            .any(|protocol| requested == protocol)
    });
    (!requested.is_empty() && !overlaps_protocol_catalog).then_some(requested)
}

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

/// A peer whose replicated collections differ from this node's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicatedSchemaSkew {
    /// Collections that are missing on either side or whose identities differ.
    pub collections: Vec<String>,
    /// `false` when the peer published no readable schema at all.
    pub advertised: bool,
    /// Differing collections this side holds off the canonical schema lineage (a store
    /// created by v1.0.0-rc1). Same release on both sides cannot fix those; re-creating
    /// this side's store can.
    pub local_off_lineage: Vec<String>,
    /// Differing collections the peer holds off the canonical schema lineage.
    pub remote_off_lineage: Vec<String>,
}

impl fmt::Display for ReplicatedSchemaSkew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.local_off_lineage.is_empty() {
            return write!(
                f,
                "this app's local store was created on a schema lineage the agent runtime \
                 does not share ({}), so their synced data is incompatible; reset this app's \
                 local data to re-create the store from this release",
                self.local_off_lineage.join(", ")
            );
        }
        if !self.remote_off_lineage.is_empty() {
            return write!(
                f,
                "the agent runtime's store was created on a schema lineage this app does not \
                 share ({}), so their synced data is incompatible; re-create the runtime's \
                 store (see docs/upgrade-from-rc0.md)",
                self.remote_off_lineage.join(", ")
            );
        }
        write!(
            f,
            "this app and the agent runtime are different Gents versions, so their synced \
             data is incompatible and writes would never reach the runtime; update the app \
             and the runtime to the same version"
        )?;
        if self.advertised {
            write!(
                f,
                " (collections that differ: {})",
                self.collections.join(", ")
            )
        } else {
            write!(f, " (the runtime does not advertise its replicated schema)")
        }
    }
}

impl std::error::Error for ReplicatedSchemaSkew {}

pub fn compare_replicated_schema(
    expected: &[&str],
    local: &ReplicatedSchema,
    remote: Option<&ReplicatedSchema>,
) -> Result<(), ReplicatedSchemaSkew> {
    let Some(remote) = remote else {
        return Err(ReplicatedSchemaSkew {
            collections: expected.iter().map(|name| (*name).to_string()).collect(),
            advertised: false,
            ..ReplicatedSchemaSkew::default()
        });
    };
    let mut collections = expected
        .iter()
        .filter(|name| match (local.get(**name), remote.get(**name)) {
            (Some(local), Some(remote)) => local != remote,
            _ => true,
        })
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    collections.sort();
    collections.dedup();
    if collections.is_empty() {
        Ok(())
    } else {
        Err(ReplicatedSchemaSkew {
            collections,
            advertised: true,
            ..ReplicatedSchemaSkew::default()
        })
    }
}
