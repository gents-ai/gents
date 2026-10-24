use super::{DocumentRecord, DocumentRuntimeView, DocumentViewError};
use crate::collection::Collection;
use crate::config_client::{config_projection, ConfigAccess, ConfigApplyTxn};
use crate::document_config::ensure_node;
use crate::graphql::escape_graphql_string;
use anyhow::Result;
use defra_node::EmbeddedNode;
use rapidhash::{HashMapExt, RapidHashMap};
use serde::de::DeserializeOwned;

pub(crate) async fn load_document_runtime_view(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<DocumentRuntimeView> {
    if node_did.trim().is_empty() {
        return Err(DocumentViewError::OwnerDidRequired.into());
    }
    ensure_node(node, node_did).await?;
    let owner = node_did.to_owned();
    let mut view = ConfigAccess::transact_local_readonly(
        node,
        None,
        "load_runtime_document_view",
        move |txn| {
            let owner = owner.clone();
            Box::pin(async move {
                let mut principals = load_records(txn, &owner, Collection::Node).await?;
                let principal = principals
                    .remove(&owner)
                    .ok_or(DocumentViewError::ScopedPrincipalMissing)?;
                let mut unknown_kind_backends = RapidHashMap::new();
                let (package_artifacts, graph_digests) =
                    crate::graph_pipeline::load_runtime_graph_artifacts_in_txn(txn, &owner).await?;
                let mut view = DocumentRuntimeView {
                    node: principal,
                    agents: load_records(txn, &owner, Collection::Agent).await?,
                    contexts: load_records(txn, &owner, Collection::AgentContext).await?,
                    compactions: load_records(txn, &owner, Collection::Compaction).await?,
                    tools: load_records(txn, &owner, Collection::Tools).await?,
                    agent_targets: load_records(txn, &owner, Collection::AgentTarget).await?,
                    skills: load_records(txn, &owner, Collection::Skill).await?,
                    datastore_tool_surfaces: load_records(
                        txn,
                        &owner,
                        Collection::DatastoreToolSurface,
                    )
                    .await?,
                    eth_tools: load_records(txn, &owner, Collection::EthTool).await?,
                    inference_profiles: load_records(txn, &owner, Collection::InferenceProfile)
                        .await?,
                    inference_sampling: load_records(txn, &owner, Collection::InferenceSampling)
                        .await?,
                    inference_execution: load_records(txn, &owner, Collection::InferenceExecution)
                        .await?,
                    inference_retry_policies: load_records(
                        txn,
                        &owner,
                        Collection::InferenceRetryPolicy,
                    )
                    .await?,
                    backends: load_records_skipping(
                        txn,
                        &owner,
                        Collection::InferenceBackend,
                        &mut unknown_kind_backends,
                    )
                    .await?,
                    tasks: load_records(txn, &owner, Collection::Task).await?,
                    schedules: load_records(txn, &owner, Collection::Schedule).await?,
                    triggers: load_records(txn, &owner, Collection::Trigger).await?,
                    event_sources: load_records(txn, &owner, Collection::EventSource).await?,
                    callbacks: load_records(txn, &owner, Collection::Callback).await?,
                    callback_bindings: load_records(txn, &owner, Collection::CallbackBinding)
                        .await?,
                    chain_key_bindings: load_records(txn, &owner, Collection::ChainKeyBinding)
                        .await?,
                    tool_services: load_records(txn, &owner, Collection::ToolServiceRegistry)
                        .await?,
                    projection_acp_bindings: load_records(
                        txn,
                        &owner,
                        Collection::ProjectionAcpBinding,
                    )
                    .await?,
                    callback_modules: load_records(txn, &owner, Collection::CallbackModule).await?,
                    repository_placements: load_records(
                        txn,
                        &owner,
                        Collection::RepositoryPlacement,
                    )
                    .await?,
                    backend_observations: RapidHashMap::new(),
                    unknown_kind_backends,
                    oauth_credentials: RapidHashMap::new(),
                };
                // Retain ordinary authored documents; reserved package and
                // graph resources execute only through their verified revision owner.
                let visible = |id: &str| {
                    (!id.starts_with("pkg-") || package_artifacts.contains(id))
                        && crate::graph_pipeline::graph_artifact_is_visible(id, &graph_digests)
                };
                macro_rules! retain_visible {
                    ($($field:ident),+ $(,)?) => { $(view.$field.retain(|id, _| visible(id));)+ };
                }
                retain_visible!(
                    agents,
                    contexts,
                    compactions,
                    tools,
                    agent_targets,
                    skills,
                    datastore_tool_surfaces,
                    eth_tools,
                    inference_profiles,
                    inference_sampling,
                    inference_execution,
                    inference_retry_policies,
                    backends,
                    unknown_kind_backends,
                    tasks,
                    schedules,
                    triggers,
                    event_sources,
                    callbacks,
                    callback_bindings,
                    chain_key_bindings,
                    tool_services,
                    projection_acp_bindings,
                    callback_modules,
                    repository_placements,
                );
                Ok(view)
            })
        },
    )
    .await?;
    for backend_id in view.backends.keys() {
        if let Some(observation) =
            crate::backend_registry::lookup_backend_observation(node, node_did, backend_id).await?
        {
            view.backend_observations
                .insert(backend_id.clone(), observation);
        }
    }
    for credential in crate::oauth_credential::list_oauth_credentials(node, node_did).await? {
        let doc_id = credential
            .doc_id
            .clone()
            .ok_or(DocumentViewError::OAuthCredentialMissingPhysicalId)?;
        if credential.node_did != node_did {
            return Err(DocumentViewError::OAuthCredentialOwnerMismatch.into());
        }
        let id = credential.credential_id.clone();
        if view
            .oauth_credentials
            .insert(
                id.clone(),
                DocumentRecord {
                    doc_id,
                    value: credential,
                },
            )
            .is_some()
        {
            return Err(DocumentViewError::DuplicateOAuthCredential { id }.into());
        }
    }
    Ok(view)
}

pub(super) fn decode_record<T>(
    name: &str,
    id: &str,
    canonical: serde_json::Value,
) -> Result<T, DocumentViewError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(canonical).map_err(|source| DocumentViewError::Decode {
        name: name.to_owned(),
        id: id.to_owned(),
        source,
    })
}

async fn load_records<T>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    collection: Collection,
) -> Result<RapidHashMap<String, DocumentRecord<T>>>
where
    T: DeserializeOwned,
{
    load_records_skipping(txn, owner, collection, &mut RapidHashMap::new()).await
}

async fn load_records_skipping<T>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    collection: Collection,
    unknown_kinds: &mut RapidHashMap<String, String>,
) -> Result<RapidHashMap<String, DocumentRecord<T>>>
where
    T: DeserializeOwned,
{
    let name = collection.graphql_type();
    let (fields, _) = config_projection(collection, None)?;
    let response = txn
        .execute(&format!(
            "{{ {name}(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID {} }} }}",
            escape_graphql_string(owner),
            fields.join(" ")
        ))
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| DocumentViewError::QueryRowsMissing {
            name: name.to_owned(),
        })?;
    let mut result = RapidHashMap::new();
    for row in rows {
        let mut row = row.clone();
        let doc_id = row
            .as_object_mut()
            .ok_or(DocumentViewError::ConfigRowNotObject)?
            .remove("_docID")
            .and_then(|id| id.as_str().map(str::to_owned))
            .ok_or(DocumentViewError::ConfigRowMissingPhysicalId)?;
        if doc_id.is_empty() {
            return Err(DocumentViewError::ConfigRowEmptyPhysicalId.into());
        }
        if row.get("node_did").and_then(serde_json::Value::as_str) != Some(owner) {
            return Err(DocumentViewError::ForeignRowEscapedOwnerFilter {
                name: name.to_owned(),
            }
            .into());
        }
        let id = row
            .get(collection.unique_field())
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| DocumentViewError::MissingLogicalKey {
                name: name.to_owned(),
            })?
            .to_owned();
        if collection == Collection::InferenceBackend {
            if let Some(kind) = crate::backend_registry::unknown_provider_kind(&row) {
                tracing::warn!(
                    backend_id = id,
                    provider_kind = kind,
                    "skipping backend of a provider kind this build does not know"
                );
                unknown_kinds.insert(id, kind.to_owned());
                continue;
            }
        }
        let (_, canonical) = config_projection(collection, Some(&row))?;
        let value = decode_record(
            name,
            &id,
            canonical.ok_or(DocumentViewError::MissingCanonicalConfig)?,
        )?;
        if result
            .insert(id.clone(), DocumentRecord { doc_id, value })
            .is_some()
        {
            return Err(DocumentViewError::AmbiguousReference {
                name: name.to_owned(),
                id,
                owner: owner.to_owned(),
            }
            .into());
        }
    }
    Ok(result)
}
