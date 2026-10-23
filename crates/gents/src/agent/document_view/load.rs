use super::{DocumentRecord, DocumentRuntimeView};
use crate::collection::Collection;
use crate::config_client::{config_projection, ConfigAccess, ConfigApplyTxn};
use crate::document_config::ensure_node;
use crate::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

pub(crate) async fn load_document_runtime_view(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<DocumentRuntimeView> {
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "runtime view requires an owner DID"
    );
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
                    .context("scoped principal is missing")?;
                let mut unknown_kind_backends = HashMap::new();
                let view = DocumentRuntimeView {
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
                    tool_services: load_records(txn, &owner, Collection::ToolServiceRegistry)
                        .await?,
                    backend_observations: HashMap::new(),
                    unknown_kind_backends,
                    oauth_credentials: HashMap::new(),
                };
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
            .context("OAuthCredential missing physical ID")?;
        anyhow::ensure!(
            credential.node_did == node_did,
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

pub(super) fn decode_record<T: DeserializeOwned>(
    name: &str,
    id: &str,
    canonical: serde_json::Value,
) -> Result<T> {
    serde_json::from_value(canonical).with_context(|| format!("decoding {name} {id:?}"))
}

async fn load_records<T: DeserializeOwned>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    collection: Collection,
) -> Result<HashMap<String, DocumentRecord<T>>> {
    load_records_skipping(txn, owner, collection, &mut HashMap::new()).await
}

async fn load_records_skipping<T: DeserializeOwned>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    collection: Collection,
    unknown_kinds: &mut HashMap<String, String>,
) -> Result<HashMap<String, DocumentRecord<T>>> {
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
            row.get("node_did").and_then(serde_json::Value::as_str) == Some(owner),
            "foreign {name} row escaped owner filter"
        );
        let id = row
            .get(collection.unique_field())
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .with_context(|| format!("{name} missing logical key"))?
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
        let value = decode_record(name, &id, canonical.context("missing canonical config")?)?;
        anyhow::ensure!(
            result
                .insert(id.clone(), DocumentRecord { doc_id, value })
                .is_none(),
            "ambiguous {name} reference {id:?} for {owner:?}"
        );
    }
    Ok(result)
}
