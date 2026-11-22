use defra_node::{CollectionVersion, Node as EmbeddedNode};
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

use super::{
    manages_client_journal, manages_file_images, manages_mailbox, BaselineCollection, Catalog,
    SchemaError, FILE_COLLECTION_POLICIES,
};

type Result<T, E = SchemaError> = std::result::Result<T, E>;

/// The read-only refusals [`super::ensure_runtime`] raises before it writes:
/// a store whose branchability or identity policies another Gents version
/// declared, for a home whose policies have `ids`. Callers decide from it
/// whether this build can take the store.
pub async fn validate_existing_schema(node: &EmbeddedNode, ids: &HomePolicyIds) -> Result<()> {
    validate_runtime_locality(node, ids)?;
    validate_bindings(node, super::runtime_catalog(ids), ids).await
}

/// Read-only preflight of the same lineages startup accepts for a home whose
/// policies have `ids`. Callers may read these versions through ConfigAccess
/// while another process owns the store.
pub fn check_lineages(all_versions: &[CollectionVersion], ids: &HomePolicyIds) -> Result<()> {
    check_lineage_versions(all_versions, super::runtime_catalog(ids))
}

/// DefraDB omits branchability from schema CIDs and rejects changes to it.
/// A matching lineage therefore cannot establish runtime-local placement. Refuse
/// an incompatible store before registering or changing any of its collections;
/// startup must never silently retain its previous replication semantics.
pub(super) fn validate_runtime_locality(node: &EmbeddedNode, ids: &HomePolicyIds) -> Result<()> {
    for entry in super::baseline(ids) {
        if gents_protocol::schemas::is_replicated_protocol_collection(entry.name) {
            continue;
        }
        if node
            .get_collection(entry.name)?
            .is_some_and(|collection| collection.is_branchable)
        {
            return Err(SchemaError::RuntimeLocalityMismatch {
                collection: entry.name.to_owned(),
            });
        }
    }
    Ok(())
}

pub(super) async fn validate_bindings(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    validate_identity_bindings(node, catalog, ids)?;
    validate_legacy_backend_bindings(node, catalog, ids).await?;
    validate_client_journal_binding(node, catalog, ids)?;
    validate_mailbox_binding(node, catalog, ids)
}

async fn validate_legacy_backend_bindings(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    use gents_protocol::schemas::INFERENCE_BACKEND_NAME;
    if !catalog.manages(INFERENCE_BACKEND_NAME) {
        return Ok(());
    }
    let active = node.get_collection(INFERENCE_BACKEND_NAME)?;
    let versions = node.get_all_collection_versions().await?;
    for version in versions.iter().filter(|version| {
        version.name == INFERENCE_BACKEND_NAME
            || active
                .as_ref()
                .is_some_and(|current| current.collection_id == version.collection_id)
    }) {
        if active.is_none()
            || version.is_branchable
            || !version.policy.as_ref().is_some_and(|policy| {
                policy.id == ids.id(BuiltInPolicy::Household)
                    && policy.resource_name == "secret_configuration"
            })
        {
            return Err(SchemaError::IncompatibleIdentityPolicy {
                collection: INFERENCE_BACKEND_NAME.into(),
            });
        }
    }
    Ok(())
}

fn validate_identity_bindings(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    for &(name, resource) in gents_protocol::schemas::IDENTITY_POLICY_BINDINGS {
        if !catalog.manages(name) {
            continue;
        }
        if let Some(collection) = node.get_collection(name)? {
            if collection.is_branchable
                != gents_protocol::schemas::is_replicated_protocol_collection(name)
                || !collection.policy.as_ref().is_some_and(|policy| {
                    policy.id == ids.id(BuiltInPolicy::Household)
                        && policy.resource_name == resource
                })
            {
                return Err(SchemaError::IncompatibleIdentityPolicy {
                    collection: name.into(),
                });
            }
        }
    }
    Ok(())
}

fn validate_mailbox_binding(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    use gents_protocol::schemas::{MAILBOX_ITEM_NAME, MAILBOX_POLICY_RESOURCE};
    if manages_mailbox(catalog) {
        if let Some(collection) = node.get_collection(MAILBOX_ITEM_NAME)? {
            if collection.is_branchable
                || !collection.policy.as_ref().is_some_and(|policy| {
                    policy.id == ids.id(BuiltInPolicy::Mailbox)
                        && policy.resource_name == MAILBOX_POLICY_RESOURCE
                })
            {
                return Err(SchemaError::IncompatibleIdentityPolicy {
                    collection: MAILBOX_ITEM_NAME.into(),
                });
            }
        }
    }
    Ok(())
}

fn validate_client_journal_binding(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    use gents_protocol::schemas::{CLIENT_JOURNAL_POLICY_RESOURCE, PENDING_CHAT_SUBMISSION_NAME};
    if manages_client_journal(catalog) {
        if let Some(collection) = node.get_collection(PENDING_CHAT_SUBMISSION_NAME)? {
            if collection.is_branchable
                || !collection.policy.as_ref().is_some_and(|policy| {
                    policy.id == ids.id(BuiltInPolicy::ClientJournal)
                        && policy.resource_name == CLIENT_JOURNAL_POLICY_RESOURCE
                })
            {
                return Err(SchemaError::IncompatibleIdentityPolicy {
                    collection: PENDING_CHAT_SUBMISSION_NAME.into(),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn validate_file_image_bindings<'n>(
    node: &'n EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<Option<&'n str>> {
    if !manages_file_images(catalog) {
        return Ok(None);
    }
    for (name, branchable) in FILE_COLLECTION_POLICIES {
        if let Some(collection) = node.get_collection(name)? {
            validate_file_image_policy(&collection, *branchable, ids)?;
        }
    }
    Ok(Some(
        node.node_identity_did()
            .ok_or(SchemaError::MissingNodeIdentity)?,
    ))
}

/// Policy bindings are outside the pinned schema version; lineage validation
/// alone cannot establish the retained images' document permissions.
pub(super) fn validate_file_image_policy(
    collection: &CollectionVersion,
    branchable: bool,
    ids: &HomePolicyIds,
) -> Result<()> {
    if collection.is_branchable != branchable
        || !collection.policy.as_ref().is_some_and(|policy| {
            policy.id == ids.id(BuiltInPolicy::ToolFileChange)
                && policy.resource_name == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE
        })
    {
        return Err(SchemaError::IncompatibleFileImagePolicy);
    }
    Ok(())
}

/// Reject a store whose existing managed collections descend from a lineage
/// this catalog does not know, before anything is registered.
pub(super) async fn reject_unknown_lineage(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
) -> Result<()> {
    let all_versions = node.get_all_collection_versions().await?;
    check_lineage_versions(&all_versions, catalog)
}

fn check_lineage_versions(all_versions: &[CollectionVersion], catalog: &Catalog<'_>) -> Result<()> {
    let by_name = versions_by_name(all_versions);
    let known_pins = known_pin_set(catalog);
    for entry in catalog.baseline {
        if let Some(versions) = by_name.get(entry.name) {
            check_known_lineage(entry, versions, &known_pins)?;
        }
    }
    Ok(())
}

pub(super) async fn verify_lineages(node: &EmbeddedNode, catalog: &Catalog<'_>) -> Result<()> {
    let all_versions = node.get_all_collection_versions().await?;
    let by_name = versions_by_name(&all_versions);
    let known_pins = known_pin_set(catalog);
    for entry in catalog.baseline {
        verify_one_collection(entry, &by_name, &known_pins)?;
    }
    Ok(())
}

fn versions_by_name(
    all_versions: &[CollectionVersion],
) -> RapidHashMap<String, Vec<&CollectionVersion>> {
    let mut by_name: RapidHashMap<String, Vec<&CollectionVersion>> = RapidHashMap::new();
    for version in all_versions {
        if version.is_placeholder || version.name.is_empty() {
            continue;
        }
        by_name
            .entry(version.name.clone())
            .or_default()
            .push(version);
    }
    by_name
}

fn known_pin_set(catalog: &Catalog<'_>) -> RapidHashSet<String> {
    let mut pins = RapidHashSet::new();
    for entry in catalog.baseline {
        if let Some(version) = entry.expected_version {
            pins.insert(version.to_string());
        }
    }
    pins
}

fn verify_one_collection(
    entry: &BaselineCollection<'_>,
    by_name: &RapidHashMap<String, Vec<&CollectionVersion>>,
    known_pins: &RapidHashSet<String>,
) -> Result<()> {
    let missing = || SchemaError::CollectionMissing {
        collection: entry.name.to_string(),
    };
    let versions = by_name.get(entry.name).ok_or_else(missing)?;
    check_known_lineage(entry, versions, known_pins)?;
    let active = versions
        .iter()
        .find(|version| version.is_active)
        .or_else(|| versions.first())
        .copied()
        .ok_or_else(missing)?;
    if let Some(expected) = entry.expected_version {
        if active.version_id != expected {
            return Err(SchemaError::VersionPinMismatch {
                collection: entry.name.to_string(),
                expected: expected.to_string(),
                actual: active.version_id.clone(),
            });
        }
    }
    Ok(())
}

/// A collection's DAG belongs to this catalog's lineage: the baseline root
/// is present and, when it has several versions, every one is a known pin.
fn check_known_lineage(
    entry: &BaselineCollection<'_>,
    versions: &[&CollectionVersion],
    known_pins: &RapidHashSet<String>,
) -> Result<()> {
    let unknown = || SchemaError::UnknownLineage {
        collection: entry.name.to_string(),
        versions: versions
            .iter()
            .map(|version| version.version_id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    };
    if let Some(root) = entry.expected_version {
        if !versions.iter().any(|version| version.version_id == root) {
            return Err(unknown());
        }
    }
    if versions.len() > 1 {
        if let Some(foreign) = versions
            .iter()
            .find(|version| !known_pins.contains(version.version_id.as_str()))
        {
            if known_pins.is_empty() {
                return Err(unknown());
            }
            return Err(SchemaError::ForeignVersion {
                collection: entry.name.to_string(),
                version_id: foreign.version_id.clone(),
            });
        }
    }
    Ok(())
}
