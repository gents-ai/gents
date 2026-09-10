use super::{DocumentRecord, DocumentRuntimeView};
use crate::collection::Collection;
use crate::config_client::{config_projection, ConfigAccess, ConfigApplyTxn};
use crate::document_config::ensure_agent_principal;
use crate::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

pub(crate) async fn load_document_runtime_view(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<DocumentRuntimeView> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "runtime view requires an owner DID"
    );
    ensure_agent_principal(node, agent_did).await?;
    let owner = agent_did.to_owned();
    let mut view =
        ConfigAccess::transact_local(node, None, "load_runtime_document_view", move |txn| {
            let owner = owner.clone();
            Box::pin(async move {
                let mut principals = load_records(txn, &owner, Collection::AgentPrincipal).await?;
                let principal = principals
                    .remove(&owner)
                    .context("scoped principal is missing")?;
                let (package_artifacts, graph_digests) =
                    crate::graph_pipeline::load_runtime_graph_artifacts_in_txn(txn, &owner).await?;
                let mut view = DocumentRuntimeView {
                    principal,
                    behaviors: load_records(txn, &owner, Collection::AgentBehavior).await?,
                    contexts: load_records(txn, &owner, Collection::AgentContext).await?,
                    compactions: load_records(txn, &owner, Collection::Compaction).await?,
                    tools: load_records(txn, &owner, Collection::Tools).await?,
                    subagent_targets: load_records(txn, &owner, Collection::SubagentTarget).await?,
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
                    backends: load_records(txn, &owner, Collection::InferenceBackend).await?,
                    tasks: load_records(txn, &owner, Collection::Task).await?,
                    schedules: load_records(txn, &owner, Collection::Schedule).await?,
                    triggers: load_records(txn, &owner, Collection::Trigger).await?,
                    event_sources: load_records(txn, &owner, Collection::EventSource).await?,
                    callbacks: load_records(txn, &owner, Collection::Callback).await?,
                    callback_bindings: load_records(txn, &owner, Collection::CallbackBinding)
                        .await?,
                    graph_definitions: load_records(txn, &owner, Collection::GraphDefinition)
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
                    backend_observations: HashMap::new(),
                    oauth_credentials: HashMap::new(),
                };
                // Retain ordinary authored documents. Reserved package and graph
                // resources execute only through their verified revision owner.
                let visible = |id: &str| {
                    (!id.starts_with("pkg-") || package_artifacts.contains(id))
                        && crate::graph_pipeline::graph_artifact_is_visible(id, &graph_digests)
                };
                macro_rules! retain_visible {
                    ($($field:ident),+ $(,)?) => { $(view.$field.retain(|id, _| visible(id));)+ };
                }
                retain_visible!(
                    behaviors,
                    contexts,
                    compactions,
                    tools,
                    subagent_targets,
                    skills,
                    datastore_tool_surfaces,
                    eth_tools,
                    inference_profiles,
                    inference_sampling,
                    inference_execution,
                    inference_retry_policies,
                    backends,
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
        })
        .await?;
    for backend_id in view.backends.keys() {
        if let Some(observation) =
            crate::backend_registry::lookup_backend_observation(node, agent_did, backend_id).await?
        {
            view.backend_observations
                .insert(backend_id.clone(), observation);
        }
    }
    for credential in crate::oauth_credential::list_oauth_credentials(node, agent_did).await? {
        let doc_id = credential
            .doc_id
            .clone()
            .context("OAuthCredential missing physical ID")?;
        anyhow::ensure!(
            credential.agent_did == agent_did,
            "OAuthCredential owner mismatch"
        );
        let id = credential.credential_id.clone();
        anyhow::ensure!(
            view.oauth_credentials
                .insert(
                    id.clone(),
                    DocumentRecord {
                        doc_id,
                        value: credential
                    }
                )
                .is_none(),
            "duplicate OAuthCredential {id}"
        );
    }
    Ok(view)
}

/// Select desired fields through the shared canonical serde codec. JSON groups
/// stay bare selections; observations never enter a strict authored document.
async fn load_records<T: DeserializeOwned>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    collection: Collection,
) -> Result<HashMap<String, DocumentRecord<T>>> {
    let name = collection.graphql_type();
    let (fields, _) = config_projection(collection, None)?;
    let response = txn
        .execute(&format!(
            "{{ {name}(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{ _docID {} }} }}",
            escape_graphql_string(owner),
            fields.join(" ")
        ))
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("{name} query did not return rows"))?;
    let mut result = HashMap::new();
    for row in rows {
        let mut row = row.clone();
        let doc_id = row
            .as_object_mut()
            .context("config row is not an object")?
            .remove("_docID")
            .and_then(|id| id.as_str().map(str::to_owned))
            .context("config row missing physical ID")?;
        anyhow::ensure!(!doc_id.is_empty(), "config row has empty physical ID");
        anyhow::ensure!(
            row.get("agent_did").and_then(serde_json::Value::as_str) == Some(owner),
            "foreign {name} row escaped owner filter"
        );
        let id = row
            .get(collection.unique_field())
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .with_context(|| format!("{name} missing logical key"))?
            .to_owned();
        let (_, canonical) = config_projection(collection, Some(&row))?;
        let value = serde_json::from_value(canonical.context("missing canonical config")?)?;
        anyhow::ensure!(
            result
                .insert(id.clone(), DocumentRecord { doc_id, value })
                .is_none(),
            "ambiguous {name} reference {id:?} for {owner:?}"
        );
    }
    Ok(result)
}
