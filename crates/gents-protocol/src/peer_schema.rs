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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPairingFilter {
    pub collection: String,
    pub field: String,
    pub value: Value,
}

/// An unknown or mutable field cannot safely constrain replicated documents.
pub fn admit_app_pairing_filters(
    collections: &BTreeSet<String>,
    immutable_fields: &BTreeSet<(String, String)>,
    filters: &[AppPairingFilter],
) -> bool {
    let mut seen = BTreeSet::new();
    filters.iter().all(|filter| {
        collections.contains(&filter.collection)
            && !filter.field.is_empty()
            && immutable_fields.contains(&(filter.collection.clone(), filter.field.clone()))
            && seen.insert(&filter.collection)
    })
}

pub fn resolve_app_pairing_filters(
    filtered: bool,
    collections: &BTreeSet<String>,
    immutable_fields: &BTreeSet<(String, String)>,
    filters: Option<&[AppPairingFilter]>,
) -> Option<Vec<AppPairingFilter>> {
    match (filtered, filters) {
        (false, None) => Some(Vec::new()),
        (true, Some(filters))
            if !filters.is_empty()
                && admit_app_pairing_filters(collections, immutable_fields, filters) =>
        {
            Some(filters.to_vec())
        }
        _ => None,
    }
}

#[cfg(test)]
mod app_pairing_filter_conformance {
    use super::*;

    fn fields(case: &Value) -> BTreeSet<(String, String)> {
        case["authority"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|field| field["immutable"].as_bool() == Some(true))
            .map(|field| {
                (
                    field["collection"].as_str().unwrap().to_owned(),
                    field["field"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn admission_matches_executable_model() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["app_pairing_filter_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let collections: BTreeSet<String> =
                serde_json::from_value(case["collections"].clone()).unwrap();
            let filters: Vec<AppPairingFilter> =
                serde_json::from_value(case["filters"].clone()).unwrap();
            let immutable = fields(case);
            assert_eq!(
                admit_app_pairing_filters(&collections, &immutable, &filters),
                case["valid"].as_bool().unwrap(),
                "{}",
                case["label"]
            );
        }
    }

    #[test]
    fn recovery_matches_executable_model() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["app_pairing_filter_route_cases"]
            .as_array()
            .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let collections: BTreeSet<String> =
                serde_json::from_value(case["collections"].clone()).unwrap();
            let filters: Option<Vec<AppPairingFilter>> =
                serde_json::from_value(case["filters"].clone()).unwrap();
            let actual = resolve_app_pairing_filters(
                case["mode"] == "filtered",
                &collections,
                &fields(case),
                filters.as_deref(),
            );
            assert_eq!(
                actual.is_some(),
                case["accepted"].as_bool().unwrap(),
                "{}",
                case["label"]
            );
        }
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
