use std::sync::Arc;
use std::time::Instant;

use defra_node::Node as EmbeddedNode;
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};
use tokio::sync::Mutex;
use tracing::{debug, info};

use super::compatibility::{
    reject_unknown_lineage, validate_bindings, validate_file_image_bindings,
    validate_file_image_policy, validate_runtime_locality, verify_lineages,
};
use super::{Catalog, SchemaError, FILE_COLLECTION_POLICIES};

type Result<T, E = SchemaError> = std::result::Result<T, E>;

/// Process-wide lock: desktop and runtime paths can race within one process.
static ENSURE_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug, thiserror::Error)]
pub enum RuntimeSchemaError {
    #[error("ensuring the runtime schema")]
    Schema(#[source] SchemaError),
    #[error("{0}")]
    Config(#[from] crate::config_client::ConfigClientError),
}

pub async fn ensure_runtime_owned(
    node: Arc<EmbeddedNode>,
) -> std::result::Result<(), RuntimeSchemaError> {
    crate::config_client::ConfigAccess::local(node)
        .run_local_owned(|node| async move {
            ensure_runtime(node.as_ref())
                .await
                .map_err(RuntimeSchemaError::Schema)
        })
        .await
}

pub async fn ensure_runtime(node: &EmbeddedNode) -> Result<()> {
    let ids = crate::local::document_acp::node_policy_ids(node);
    validate_runtime_locality(node, ids)?;
    ensure(node, super::runtime_catalog(ids)).await?;
    validate_runtime_locality(node, ids)
}

/// Registers the catalog's missing collections, the policies and owner
/// objects they need and their ordinary indexes, under the policy IDs of the
/// home `node` serves. Every refusal is read-only and precedes the first
/// write.
pub async fn ensure(node: &EmbeddedNode, catalog: &Catalog<'_>) -> Result<()> {
    let _guard = ENSURE_LOCK.lock().await;
    let started = Instant::now();
    let ids = crate::local::document_acp::node_policy_ids(node);
    validate_bindings(node, catalog, ids).await?;
    reject_unknown_lineage(node, catalog).await?;
    let file_image_owner = validate_file_image_bindings(node, catalog, ids)?;
    super::node_policies::register(node, catalog, ids).await?;
    let registered = register_baseline(node, catalog, ids).await?;
    register_branchable_identity_collection_owners(node, catalog, ids).await?;
    if let Some(owner) = file_image_owner {
        register_file_image_owner(node, owner, catalog, ids).await?;
    }
    verify_lineages(node, catalog).await?;
    reconcile_ordinary_indexes(node, catalog).await?;
    info!(
        baseline_registered = registered,
        baseline_already_present = catalog.baseline.len() - registered,
        elapsed_ms = started.elapsed().as_millis(),
        "schema ensured"
    );
    Ok(())
}

async fn register_baseline(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<usize> {
    let mut missing = Vec::new();
    for entry in catalog.baseline {
        if node.get_collection(entry.name)?.is_none() {
            missing.push(entry);
        }
    }
    if missing.is_empty() {
        return Ok(0);
    }
    let batch = ids.render(
        &missing
            .iter()
            .map(|entry| entry.sdl)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    node.add_collection(&batch)
        .await
        .map_err(|source| SchemaError::BaselineRegister {
            collection: missing
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(","),
            source: source.into(),
        })?;
    debug!(
        collections = missing.len(),
        "baseline schemas registered in one batch"
    );
    Ok(missing.len())
}

/// Branchable reads fall back to a collection ACP object when a document has
/// no registered genesis. The local runtime owns that object so missing or
/// unverified registrations do not become public reads.
async fn register_branchable_identity_collection_owners(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    use gents_protocol::schemas::IDENTITY_POLICY_BINDINGS;
    let household = ids.id(BuiltInPolicy::Household);
    let owned: Vec<_> = IDENTITY_POLICY_BINDINGS
        .iter()
        .filter(|(name, _)| {
            gents_protocol::schemas::is_replicated_protocol_collection(name)
                && catalog.manages(name)
        })
        .collect();
    if owned.is_empty() {
        return Ok(());
    }
    let runtime = identity::Did::new(
        node.node_identity_did()
            .ok_or(SchemaError::MissingNodeIdentity)?,
    )
    .map_err(|error| SchemaError::InvalidIdentityCollectionOwner { error })?;
    let acp = node.document_acp();
    for (name, resource) in owned {
        let collection =
            node.get_collection(name)?
                .ok_or_else(|| SchemaError::CollectionMissing {
                    collection: (*name).to_owned(),
                })?;
        if acp
            .is_doc_registered(household, resource, &collection.collection_id)
            .await
            .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?
        {
            let owner = acp
                .get_doc_owner(household, resource, &collection.collection_id)
                .await
                .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?;
            if owner.as_ref() != Some(&runtime) {
                return Err(SchemaError::ForeignIdentityCollectionOwner {
                    collection: (*name).to_owned(),
                });
            }
        } else {
            acp.register_object(&runtime, household, resource, &collection.collection_id)
                .await
                .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?;
        }
    }
    Ok(())
}

/// DefraDB's branchable read guard falls back to the collection object when a
/// document has no local ACP registration. That fallback must remain private.
async fn register_file_image_owner(
    node: &EmbeddedNode,
    owner: &str,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    let owner = owner
        .parse::<identity::Did>()
        .map_err(|error| SchemaError::InvalidImageOwner { error })?;
    for (name, branchable) in FILE_COLLECTION_POLICIES {
        if !catalog.manages(name) {
            continue;
        }
        let collection =
            node.get_collection(name)?
                .ok_or_else(|| SchemaError::CollectionMissing {
                    collection: (*name).into(),
                })?;
        validate_file_image_policy(&collection, *branchable, ids)?;
        // Vera checks the document itself for a non-branchable read. Registering
        // the schema ID would claim the same object on every device.
        if !collection.is_branchable
            && crate::local::document_acp::provider_of(node)
                == crate::local::document_acp::DocumentAcp::Vera
        {
            continue;
        }
        let acp = node.document_acp();
        let policy = ids.id(BuiltInPolicy::ToolFileChange);
        let resource = gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE;
        if acp
            .is_doc_registered(policy, resource, &collection.collection_id)
            .await
            .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?
        {
            let existing_owner = acp
                .get_doc_owner(policy, resource, &collection.collection_id)
                .await
                .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?;
            if existing_owner.as_ref() != Some(&owner) {
                return Err(SchemaError::ForeignFileImageOwner);
            }
        } else {
            acp.register_object(&owner, policy, resource, &collection.collection_id)
                .await
                .map_err(|error| SchemaError::from(defra_node::Error::from(error)))?;
        }
    }
    Ok(())
}

async fn reconcile_ordinary_indexes(node: &EmbeddedNode, catalog: &Catalog<'_>) -> Result<()> {
    for index in catalog.ordinary_indexes {
        let fields = index
            .fields
            .iter()
            .map(|field| schema::IndexedFieldDescription {
                name: field.name.to_owned(),
                descending: field.descending,
            })
            .collect();
        node.ensure_ordinary_index(index.collection, index.name, fields)
            .await?;
    }
    Ok(())
}
