//! The active collection versions a runtime publishes on its `/status`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level `/status` field carrying the runtime's [`ReplicatedSchema`] for
/// the client route. `null` means the runtime could not read its own node yet.
pub const STATUS_REPLICATED_SCHEMA_FIELD: &str = "replicated_schema";

pub fn is_managed_collection(name: &str) -> bool {
    crate::schemas::ALL_COLLECTION_NAMES.contains(&name)
        || crate::schemas::RUNTIME_COLLECTION_NAMES.contains(&name)
}

pub const P2P_PAIRING_SOURCE_PURPOSE: &str = "p2p-pairing";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppCollectionFacts {
    pub managed: bool,
    pub has_policy: bool,
    pub is_view: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AppCollectionRefusal {
    #[error("protocol, runtime and credential collections never replicate as application data")]
    Managed,
    #[error(
        "a collection with a document policy cannot replicate: its replicas are owned by \
         their signer on the peer and grants do not replicate, so only that signer could read them"
    )]
    PolicyProtected,
    #[error("a view is derived from other collections and cannot carry its own document policy")]
    View,
}

pub fn decide_app_collection(facts: AppCollectionFacts) -> Result<(), AppCollectionRefusal> {
    if facts.managed {
        Err(AppCollectionRefusal::Managed)
    } else if facts.is_view {
        Err(AppCollectionRefusal::View)
    } else if facts.has_policy {
        Err(AppCollectionRefusal::PolicyProtected)
    } else {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families;

    #[test]
    fn app_collection_admission_follows_the_model() {
        for case in families::application_pairing_collection_admission_cases::cases::<String>() {
            let actual = decide_app_collection(AppCollectionFacts {
                managed: case.input.managed,
                has_policy: case.input.has_policy,
                is_view: case.input.is_view,
            });
            case.check(actual.err().map(|refusal| match refusal {
                AppCollectionRefusal::Managed => "managed".to_owned(),
                AppCollectionRefusal::PolicyProtected => "policy_protected".to_owned(),
                AppCollectionRefusal::View => "view".to_owned(),
            }));
        }
    }
}
