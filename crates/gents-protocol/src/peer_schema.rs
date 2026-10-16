//! Live compatibility of the collection versions two running nodes replicate.
//! DefraDB resolves every merged block by `schema_version_id` and rejects
//! versions the receiver does not hold; a peer whose active versions differ can
//! never merge the other side's writes and must be reported instead.

use std::collections::BTreeMap;
use std::fmt;

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

/// A peer whose replicated collections differ from this node's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicatedSchemaSkew {
    /// Collections that are missing on either side or whose identities differ.
    pub collections: Vec<String>,
    /// `false` when the peer published no readable schema at all.
    pub advertised: bool,
}

impl fmt::Display for ReplicatedSchemaSkew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
        })
    }
}

