use rapidhash::{HashSetExt, RapidHashSet};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use gents_documents::callback::validate_callback_binding;
use gents_store::graphql::{
    escape_graphql_string, first_row, graphql_with_transaction_retry, rows,
};

use gents_session::workspace::{
    IsolatedWorkspaceDoc, MemoryWorkspaceDocuments, RepositoryPlacementRef, WorkspaceDocuments,
    WorkspacePlacementDoc,
};

pub(crate) const SUCCEEDED_REPAIR_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const SUCCEEDED_REPAIR_LIMIT: u32 = 256;

const INVOCATION_FIELDS: &str = "invocation_id owner_node_did agent_did callback_id configuration_generation claimed_callback origin input idempotency_key caused_by_correlation lifecycle_state attempts action_plan action_journal error claimed_at created_at";
const RESULT_FIELDS: &str = "result_id invocation_id binding_id owner_node_did agent_did workspace_id work_unit_id caused_by_correlation created_at";

const ISOLATED_WORKSPACE_FIELDS: &str = r#"
    workspace_id
    work_unit_id
    repository_id
    base_sha
    branch
    creation_policy
    adapter
    owner_node_did
    writer_principal
    integrator_principal
    instruction_manifest
    path_capability
    seal_hash
    lifecycle_state
    caused_by_invocation_id
    caused_by_correlation
    handoff_id
    reply_session_id
    attempt
"#;

const PLACEMENT_FIELDS: &str = r#"
    workspace_id
    owner_node_did
    host_path
    repository_placement_id
    adapter
    adapter_version
    dirty_base
    dirty_base_summary
    provisioning_state
    observed_tree_hash
"#;

pub use gents_documents::{
    CallbackBinding as CallbackBindingDoc, CallbackModule as CallbackModuleDoc,
};

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackInvocationDoc {
    /// Immutable projected planner input captured before dispatch; retries
    /// never reread changed source rows or recompute a persisted action plan.
    #[serde(deserialize_with = "deserialize_callback_input")]
    pub input: serde_json::Value,
    pub invocation_id: String,
    pub owner_node_did: String,
    pub agent_did: String,
    pub callback_id: String,
    #[serde(default)]
    pub configuration_generation: Option<String>,
    #[serde(default)]
    pub claimed_callback: Option<String>,
    pub origin: gents_documents::CallbackInvocationOrigin,
    pub idempotency_key: String,
    #[serde(default)]
    pub caused_by_correlation: Option<String>,
    pub lifecycle_state: String,
    #[serde(default)]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub action_plan: Option<String>,
    #[serde(default)]
    pub action_journal: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct CallbackConfigDocIds {
    pub binding: String,
    pub source: String,
    pub callback: String,
}

pub(super) fn configuration_generation(
    binding: &gents_documents::CallbackBinding,
    source: &gents_documents::EventSource,
    callback: &gents_documents::Callback,
    ids: &CallbackConfigDocIds,
) -> Result<String> {
    let bytes = serde_json::to_vec(&(binding, source, callback, ids))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

const CALLBACK_INPUT_ENVELOPE_VERSION: &str = "_gents_callback_input_version";
const CALLBACK_INPUT_ENVELOPE_VALUE: &str = "value";

fn callback_input_for_storage(input: &Value) -> Value {
    if input.is_array() {
        serde_json::json!({
            CALLBACK_INPUT_ENVELOPE_VERSION: 1,
            CALLBACK_INPUT_ENVELOPE_VALUE: input,
        })
    } else {
        input.clone()
    }
}

pub(super) fn callback_input_from_storage(mut input: Value) -> Value {
    let Some(object) = input.as_object_mut() else {
        return input;
    };
    let is_envelope = object.len() == 2
        && object
            .get(CALLBACK_INPUT_ENVELOPE_VERSION)
            .and_then(Value::as_u64)
            == Some(1)
        && object.contains_key(CALLBACK_INPUT_ENVELOPE_VALUE);
    if is_envelope {
        object
            .remove(CALLBACK_INPUT_ENVELOPE_VALUE)
            .unwrap_or(Value::Null)
    } else {
        input
    }
}

fn deserialize_callback_input<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(callback_input_from_storage)
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackResultDoc {
    pub result_id: String,
    pub invocation_id: String,
    #[serde(default)]
    pub binding_id: Option<String>,
    pub owner_node_did: String,
    pub agent_did: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub work_unit_id: Option<String>,
    #[serde(default)]
    pub caused_by_correlation: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Stamped from the invocation's frozen input so a consumer Agent reads
    /// the route from this result, not from the producer's invocation.
    #[serde(flatten)]
    pub route: gents_protocol::trigger_delivery::HandoffRoute,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CallbackResultInvocationRow {
    pub invocation_id: String,
    #[serde(default)]
    pub parent_linked: bool,
}

async fn load_config<T>(
    node: &std::sync::Arc<EmbeddedNode>,
    collection: gents_store::collection::Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned + Send,
{
    Ok(load_config_record(node, collection, owner, id)
        .await?
        .map(|(_, value)| value))
}

pub(super) async fn load_config_record<T>(
    node: &std::sync::Arc<EmbeddedNode>,
    collection: gents_store::collection::Collection,
    owner: &str,
    id: &str,
) -> Result<Option<(String, T)>>
where
    T: serde::de::DeserializeOwned + Send,
{
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "callback.read_config",
        |txn| {
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(txn, collection, owner, id)
                    .await?
                    .map(|(doc, value)| {
                        serde_json::from_value(value)
                            .map(|value| (doc, value))
                            .map_err(Into::into)
                    })
                    .transpose()
            })
        },
    )
    .await
}

pub async fn list_enabled_bindings(
    node: &EmbeddedNode,
    owner: &str,
) -> Result<Vec<CallbackBindingDoc>> {
    let (fields, _) = gents_documents::config_projection(
        gents_store::collection::Collection::CallbackBinding,
        None,
    )?;
    let query = format!(
        "{{ CallbackBinding(filter: {{ node_did: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
        escape_graphql_string(owner),
        fields.join(" ")
    );
    let response = graphql_with_transaction_retry(node, &query, "query CallbackBinding").await?;
    let mut bindings: Vec<CallbackBindingDoc> = rows(&response, "CallbackBinding")?;
    let mut ids = RapidHashSet::new();
    for binding in &bindings {
        anyhow::ensure!(
            binding.node_did == owner && ids.insert(&binding.binding_id),
            "ambiguous or foreign CallbackBinding"
        );
        validate_callback_binding(binding)?;
    }
    bindings.retain(|binding| {
        binding.enabled && !gents_protocol::canonical::is_retired_reserved_id(&binding.binding_id)
    });
    bindings.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
    Ok(bindings)
}

#[cfg(test)]
pub async fn load_callback_module(
    node: &std::sync::Arc<EmbeddedNode>,
    module_id: &str,
    owner: &str,
) -> Result<Option<CallbackModuleDoc>> {
    load_config(
        node,
        gents_store::collection::Collection::CallbackModule,
        owner,
        module_id,
    )
    .await
}

pub(super) async fn load_callback_module_as(
    access: &gents_store::config_client::ConfigAccess,
    module_id: &str,
    owner: &str,
) -> Result<Option<(String, CallbackModuleDoc)>> {
    access
        .transact_readonly("callback.read_module_as_agent", |txn| {
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::CallbackModule,
                    owner,
                    module_id,
                )
                .await?
                .map(|(doc, value)| {
                    serde_json::from_value(value)
                        .map(|value| (doc, value))
                        .map_err(Into::into)
                })
                .transpose()
            })
        })
        .await
}
pub async fn load_callback(
    node: &std::sync::Arc<EmbeddedNode>,
    callback_id: &str,
    owner: &str,
) -> Result<Option<gents_documents::Callback>> {
    load_config(
        node,
        gents_store::collection::Collection::Callback,
        owner,
        callback_id,
    )
    .await
}
#[cfg(test)]
pub async fn load_binding(
    node: &std::sync::Arc<EmbeddedNode>,
    binding_id: &str,
    owner: &str,
) -> Result<Option<CallbackBindingDoc>> {
    load_config(
        node,
        gents_store::collection::Collection::CallbackBinding,
        owner,
        binding_id,
    )
    .await
}
pub async fn load_event_source(
    node: &std::sync::Arc<EmbeddedNode>,
    source_id: &str,
    owner: &str,
) -> Result<Option<gents_documents::EventSource>> {
    load_config(
        node,
        gents_store::collection::Collection::EventSource,
        owner,
        source_id,
    )
    .await
}

pub async fn load_trusted_callback_signers(node: &EmbeddedNode) -> Result<BTreeSet<String>> {
    let query = r#"{
        Node(filter: { enabled: { _eq: true } }) {
            node_did
        }
    }"#;
    let response =
        graphql_with_transaction_retry(node, query, "query trusted Node signers").await?;
    let rows: Vec<Value> = rows(&response, "Node")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.get("node_did")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|did| !did.is_empty())
                .map(str::to_string)
        })
        .collect())
}

pub async fn load_invocation(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ invocation_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        id = escape_graphql_string(invocation_id),
        owner = escape_graphql_string(owner),
    );
    let response = callback_query_as(node, agent_did, &query, "callback.read_invocation")
        .await
        .with_context(|| format!("query CallbackInvocation {invocation_id}"))?;
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackInvocation").map(|rows| rows.len())? <= 1,
        "ambiguous node-scoped callback/workspace document"
    );
    Ok(first_row(&response, "CallbackInvocation")?)
}

pub async fn load_invocation_by_key(
    node: &std::sync::Arc<EmbeddedNode>,
    key: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ idempotency_key: {{ _eq: "{key}" }}, owner_node_did: {{ _eq: "{owner}" }}, agent_did: {{ _eq: "{agent}" }} }},
                limit: 2
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        key = escape_graphql_string(key),
        owner = escape_graphql_string(owner),
        agent = escape_graphql_string(agent_did),
    );
    let response =
        callback_query_as(node, agent_did, &query, "callback.read_invocation_by_key").await?;
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackInvocation").map(|rows| rows.len())? <= 1,
        "ambiguous node-scoped callback/workspace document"
    );
    Ok(first_row(&response, "CallbackInvocation")?)
}

pub(super) async fn list_recoverable_invocations(
    node: &std::sync::Arc<EmbeddedNode>,
    owner_node_did: &str,
    engine: &super::CallbackEngine,
) -> Result<Vec<CallbackInvocationDoc>> {
    let mut invocations = Vec::new();
    for agent in callback_agents(node, owner_node_did).await? {
        let access = match engine.access_for_agent(&agent) {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(agent_did = %agent, %error, "callback recovery agent identity unavailable");
                continue;
            }
        };
        let mut repair_failed = false;
        for collection in ["CallbackInvocation", "CallbackResult"] {
            if let Err(error) =
                repair_unlinked_children(node, &access, owner_node_did, &agent, collection).await
            {
                tracing::warn!(agent_did = %agent, %collection, %error, "callback parent repair remains pending");
                repair_failed = true;
                break;
            }
        }
        if repair_failed {
            continue;
        }
        invocations.extend(
            query_owner_invocations(
                node,
                owner_node_did,
                &agent,
                r#"["pending", "claimed", "running"]"#,
            )
            .await?,
        );
        invocations.extend(
            list_recent_succeeded_missing_result(node, &access, owner_node_did, &agent).await?,
        );
    }
    Ok(invocations)
}

async fn repair_unlinked_children(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    agent_did: &str,
    collection: &str,
) -> Result<()> {
    anyhow::ensure!(
        matches!(collection, "CallbackInvocation" | "CallbackResult"),
        "invalid callback child collection"
    );
    let mut after = String::new();
    loop {
        let query = format!(
            "{{ {collection}(filter: {{owner_node_did: {{_eq: \"{}\"}}, agent_did: {{_eq: \"{}\"}}, parent_linked: {{_eq: false}}, _docID: {{_gt: \"{}\"}}}}, order: {{_docID: ASC}}, limit: 256) {{_docID invocation_id}} }}",
            escape_graphql_string(owner), escape_graphql_string(agent_did), escape_graphql_string(&after),
        );
        let response = callback_query_as(node, agent_did, &query, "callback.list_unlinked").await?;
        let rows: Vec<Value> = rows(&response, collection)?;
        for row in &rows {
            let doc_id = row["_docID"]
                .as_str()
                .filter(|id| *id > after.as_str())
                .context("callback parent repair did not advance")?;
            let invocation_id = row["invocation_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("callback parent repair lacks invocation ID")?;
            ensure_callback_parent(node, access, collection, invocation_id, owner, agent_did)
                .await?;
            after = doc_id.to_owned();
        }
        if rows.len() < 256 {
            return Ok(());
        }
    }
}

async fn list_recent_succeeded_missing_result(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    owner_node_did: &str,
    agent_did: &str,
) -> Result<Vec<CallbackInvocationDoc>> {
    let cutoff = succeeded_repair_cutoff(chrono::Utc::now());
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{
                    owner_node_did: {{ _eq: "{owner}" }},
                    agent_did: {{ _eq: "{agent}" }},
                    lifecycle_state: {{ _eq: "succeeded" }},
                    created_at: {{ _ge: "{cutoff}" }}
                }},
                order: {{ created_at: DESC }},
                limit: {limit}
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        owner = escape_graphql_string(owner_node_did),
        agent = escape_graphql_string(agent_did),
        cutoff = escape_graphql_string(&cutoff),
        limit = SUCCEEDED_REPAIR_LIMIT,
    );
    let response = callback_query_as(node, agent_did, &query, "callback.recent_succeeded").await?;
    let succeeded: Vec<CallbackInvocationDoc> = rows(&response, "CallbackInvocation")?;
    if succeeded.is_empty() {
        return Ok(Vec::new());
    }
    let results = load_callback_results_for_invocations(
        node,
        access,
        succeeded.iter().map(|row| row.invocation_id.as_str()),
        owner_node_did,
        agent_did,
    )
    .await?;
    Ok(succeeded_missing_result(succeeded, &results))
}

pub(super) async fn list_recent_failed(
    node: &std::sync::Arc<EmbeddedNode>,
    owner_node_did: &str,
    engine: &super::CallbackEngine,
) -> Result<Vec<CallbackInvocationDoc>> {
    let cutoff = succeeded_repair_cutoff(chrono::Utc::now());
    let mut failed = Vec::new();
    for agent in callback_agents(node, owner_node_did).await? {
        if let Err(error) = engine.access_for_agent(&agent) {
            tracing::warn!(agent_did = %agent, %error, "callback retry agent identity unavailable");
            continue;
        }
        let query = format!(
            r#"{{
            CallbackInvocation(
                filter: {{
                    owner_node_did: {{ _eq: "{owner}" }},
                    agent_did: {{ _eq: "{agent}" }},
                    lifecycle_state: {{ _eq: "failed" }},
                    created_at: {{ _ge: "{cutoff}" }}
                }},
                order: {{ created_at: DESC }},
                limit: {limit}
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
            owner = escape_graphql_string(owner_node_did),
            agent = escape_graphql_string(&agent),
            cutoff = escape_graphql_string(&cutoff),
            limit = SUCCEEDED_REPAIR_LIMIT,
        );
        let response = callback_query_as(node, &agent, &query, "callback.list_failed").await?;
        failed.extend(rows::<CallbackInvocationDoc>(
            &response,
            "CallbackInvocation",
        )?);
    }
    Ok(failed)
}

pub(crate) fn succeeded_repair_cutoff(now: chrono::DateTime<chrono::Utc>) -> String {
    let start = now
        - chrono::Duration::from_std(SUCCEEDED_REPAIR_WINDOW)
            .unwrap_or_else(|_| chrono::Duration::hours(24));
    start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn succeeded_missing_result(
    succeeded: Vec<CallbackInvocationDoc>,
    result_invocation_ids: &RapidHashSet<String>,
) -> Vec<CallbackInvocationDoc> {
    succeeded
        .into_iter()
        .filter(|row| !result_invocation_ids.contains(&row.invocation_id))
        .collect()
}

async fn load_callback_results_for_invocations(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    invocation_ids: impl IntoIterator<Item = &str>,
    owner: &str,
    agent_did: &str,
) -> Result<RapidHashSet<String>> {
    let ids: Vec<String> = invocation_ids
        .into_iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect();
    if ids.is_empty() {
        return Ok(RapidHashSet::new());
    }
    let query = format!(
        r#"{{
            CallbackResult(
                filter: {{ invocation_id: {{ _in: [{ids}] }}, owner_node_did: {{ _eq: "{owner}" }}, agent_did: {{ _eq: "{agent}" }} }},
                limit: {limit}
            ) {{ invocation_id parent_linked }}
        }}"#,
        ids = ids.join(", "),
        owner = escape_graphql_string(owner),
        agent = escape_graphql_string(agent_did),
        limit = SUCCEEDED_REPAIR_LIMIT,
    );
    let response =
        callback_query_as(node, agent_did, &query, "callback.results_for_repair").await?;
    let rows: Vec<CallbackResultInvocationRow> = rows(&response, "CallbackResult")?;
    let mut complete = RapidHashSet::new();
    for row in rows {
        if !row.parent_linked {
            ensure_callback_parent(
                node,
                access,
                "CallbackResult",
                &row.invocation_id,
                owner,
                agent_did,
            )
            .await?;
        }
        complete.insert(row.invocation_id);
    }
    Ok(complete)
}

async fn query_owner_invocations(
    node: &std::sync::Arc<EmbeddedNode>,
    owner_node_did: &str,
    agent_did: &str,
    states: &str,
) -> Result<Vec<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{
                    owner_node_did: {{ _eq: "{owner}" }},
                    agent_did: {{ _eq: "{agent}" }},
                    lifecycle_state: {{ _in: {states} }}
                }},
                order: {{ created_at: ASC }}
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        owner = escape_graphql_string(owner_node_did),
        agent = escape_graphql_string(agent_did),
        states = states,
    );
    let response = callback_query_as(node, agent_did, &query, "callback.list_invocations")
        .await
        .with_context(|| format!("query CallbackInvocation states {states}"))?;
    Ok(rows(&response, "CallbackInvocation")?)
}

async fn callback_agents(node: &EmbeddedNode, owner: &str) -> Result<Vec<String>> {
    let query = format!(
        "{{ Agent(filter: {{ node_did: {{ _eq: \"{}\" }} }}) {{ agent_did }} }}",
        escape_graphql_string(owner)
    );
    let response = graphql_with_transaction_retry(node, &query, "callback.list_agents").await?;
    rows::<Value>(&response, "Agent")?
        .into_iter()
        .map(|row| {
            row["agent_did"]
                .as_str()
                .filter(|did| !did.is_empty())
                .map(str::to_owned)
                .context("callback Agent has no DID")
        })
        .collect()
}

async fn callback_query_as(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    query: &str,
    operation: &'static str,
) -> Result<defra_node::QueryResponse> {
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(identity::Did::new(agent_did.to_owned())?),
        operation,
        |txn| Box::pin(async move { txn.execute_local_response(query).await }),
    )
    .await
    .map_err(Into::into)
}

pub async fn load_callback_result(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<CallbackResultDoc>> {
    let query = format!(
        r#"{{
            CallbackResult(
                filter: {{ invocation_id: {{ _eq: "{id}" }}, owner_node_did: {{ _eq: "{owner}" }}, agent_did: {{ _eq: "{agent}" }} }},
                limit: 2
            ) {{ {RESULT_FIELDS} }}
        }}"#,
        id = escape_graphql_string(invocation_id),
        owner = escape_graphql_string(owner),
        agent = escape_graphql_string(agent_did),
    );
    let response = callback_query_as(node, agent_did, &query, "callback.read_result").await?;
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackResult").map(|rows| rows.len())? <= 1,
        "ambiguous node-scoped callback/workspace document"
    );
    Ok(first_row(&response, "CallbackResult")?)
}

pub async fn create_pending_invocation(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    invocation: &CallbackInvocationDoc,
) -> Result<CallbackInvocationDoc> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(invocation.agent_did.as_str()),
        "callback invocation actor mismatch"
    );
    if let Some(existing) = load_invocation_by_key(
        node,
        &invocation.idempotency_key,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?
    {
        ensure_callback_parent(
            node,
            access,
            "CallbackInvocation",
            &existing.invocation_id,
            &existing.owner_node_did,
            &existing.agent_did,
        )
        .await?;
        return Ok(existing);
    }
    let now = invocation
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let input = serde_json::json!({
        "invocation_id": invocation.invocation_id, "owner_node_did": invocation.owner_node_did,
        "agent_did": invocation.agent_did,
        "callback_id": invocation.callback_id, "origin": invocation.origin,
        "configuration_generation": invocation.configuration_generation,
        "claimed_callback": null,
        "input": callback_input_for_storage(&invocation.input),
        "idempotency_key": invocation.idempotency_key,
        "caused_by_correlation": invocation.caused_by_correlation,
        "lifecycle_state": "pending", "attempts": 0,
        "action_plan": null, "action_journal": "[]", "error": null, "claimed_at": null,
        "created_at": now, "parent_linked": true
    });
    let mutation = "mutation($input: CallbackInvocationMutationInputArg!) { create_CallbackInvocation(input: $input) { _docID } }";
    let written = access
        .transact_write("callback.create_invocation", |txn| {
            let input = &input;
            Box::pin(async move {
                let parent = callback_agent_parent_in_txn(
                    txn,
                    &invocation.owner_node_did,
                    &invocation.agent_did,
                )
                .await?;
                let mut variables = serde_json::json!({"input": input});
                variables["input"]["agent_doc_id"] = parent.clone().into();
                let response = txn.execute_with_variables(mutation, &variables).await?;
                stage_callback_child_parent(txn, "CallbackInvocation", &response, &parent).await?;
                Ok::<_, anyhow::Error>(response)
            })
        })
        .await;
    let stored = match written {
        Ok(_) => load_invocation_by_key(
            node,
            &invocation.idempotency_key,
            &invocation.owner_node_did,
            &invocation.agent_did,
        )
        .await?
        .ok_or_else(|| anyhow!("created CallbackInvocation missing after write")),
        Err(error) => {
            if let Some(existing) = load_invocation_by_key(
                node,
                &invocation.idempotency_key,
                &invocation.owner_node_did,
                &invocation.agent_did,
            )
            .await?
            {
                ensure_callback_parent(
                    node,
                    access,
                    "CallbackInvocation",
                    &existing.invocation_id,
                    &existing.owner_node_did,
                    &existing.agent_did,
                )
                .await?;
                return Ok(existing);
            }
            Err(error).context("create_CallbackInvocation")
        }
    }?;
    Ok(stored)
}

pub(super) async fn stage_callback_child_parent(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    response: &Value,
    parent: &str,
) -> Result<()> {
    anyhow::ensure!(
        matches!(collection, "CallbackInvocation" | "CallbackResult"),
        "invalid callback child collection"
    );
    let doc = gents_store::graphql::created_doc_id(response, collection)?;
    txn.add_resource_relationship(collection, &doc, "parent", "agents", parent)
        .await?;
    Ok(())
}

/// Resolve the configured Agent while writing as its runtime-managed actor.
pub(super) async fn callback_agent_parent_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<String> {
    anyhow::ensure!(
        txn.actor_did() == Some(agent_did),
        "callback creation requires the selected agent as its authenticated actor"
    );
    use gents_protocol::document_query::{fields::agent as fields, Filter, Query, Selection};
    let rendered = Query::select(Selection::doc_id())
        .filter(
            Filter::eq(fields::NODE_DID, node_did).and(Filter::eq(fields::AGENT_DID, agent_did)),
        )
        .limit(2)
        .render();
    let response = txn.run(&rendered).await?;
    let rows = response["data"]["Agent"]
        .as_array()
        .context("callback Agent lookup omitted its result")?;
    let [agent] = rows.as_slice() else {
        anyhow::bail!("callback requires one visible physical Agent");
    };
    Ok(agent["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("callback Agent has no physical document ID")?
        .to_owned())
}

/// ACP1 creates the document before its owner grants the parent relation.
/// The marker schedules idempotent repair after interruption; authorization is
/// always decided by DefraDB. This trial assumes node-admitted writers are
/// trusted because ACP1 does not bind the creator to the authored agent field.
pub(super) async fn ensure_callback_parent(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    collection: &str,
    invocation_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<()> {
    anyhow::ensure!(
        matches!(collection, "CallbackInvocation" | "CallbackResult"),
        "invalid callback child collection"
    );
    let query = format!(
        "{{ {collection}(filter: {{invocation_id: {{_eq: \"{}\"}}, owner_node_did: {{_eq: \"{}\"}}, agent_did: {{_eq: \"{}\"}}}}, limit: 2) {{_docID agent_doc_id parent_linked}} }}",
        escape_graphql_string(invocation_id), escape_graphql_string(owner), escape_graphql_string(agent_did),
    );
    let response = callback_query_as(node, agent_did, &query, "callback.read_parent_link").await?;
    let rows: Vec<Value> = rows(&response, collection)?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("callback child is missing or ambiguous");
    };
    if row["parent_linked"].as_bool() == Some(true) {
        return Ok(());
    }
    let doc_id = row["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("callback child has no document ID")?;
    let parent = row["agent_doc_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("callback child has no Agent parent")?;
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent_did),
        "callback parent actor mismatch"
    );
    // The Agent creator grants and marks in one native transaction; outside a
    // transaction native relationship changes also require `get-collection`,
    // which an Agent does not hold (`Identity.Household.recoverParent`).
    access
        .transact_write("callback.publish_parent", |txn| {
            Box::pin(async move {
                let expected = callback_agent_parent_in_txn(txn, owner, agent_did).await?;
                anyhow::ensure!(parent == expected, "callback Agent parent changed");
                txn.add_resource_relationship(collection, doc_id, "parent", "agents", parent)
                    .await?;
                let marked = txn.execute(&format!(
                    "mutation {{ update_{collection}(docID: \"{}\", input: {{parent_linked: true}}) {{ _docID }} }}",
                    escape_graphql_string(doc_id),
                )).await?;
                anyhow::ensure!(
                    marked["data"][format!("update_{collection}")]
                        .as_array()
                        .is_some_and(|rows| rows.len() == 1),
                    "callback child disappeared before parent repair was recorded"
                );
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
}

pub(super) fn update_invocation_mutation(
    invocation: &CallbackInvocationDoc,
    expected_state: Option<&str>,
) -> String {
    let state_filter = expected_state
        .map(|state| {
            format!(
                r#", lifecycle_state: {{ _eq: "{}" }}"#,
                escape_graphql_string(state)
            )
        })
        .unwrap_or_default();
    let journal = invocation.action_journal.as_deref().unwrap_or("[]");
    format!(
        r#"mutation {{
            update_CallbackInvocation(
                filter: {{
                    invocation_id: {{ _eq: "{id}" }},
                    owner_node_did: {{ _eq: "{owner}" }},
                    agent_did: {{ _eq: "{agent}" }}
                    {state_filter}
                }},
                input: {{
                    lifecycle_state: "{state}",
                    attempts: {attempts},
                    {claimed_callback}
                    {plan}
                    action_journal: "{journal}",
                    {error}
                    {claimed_at}
                }}
            ) {{ _docID }}
        }}"#,
        id = escape_graphql_string(&invocation.invocation_id),
        owner = escape_graphql_string(&invocation.owner_node_did),
        agent = escape_graphql_string(&invocation.agent_did),
        state = escape_graphql_string(&invocation.lifecycle_state),
        attempts = invocation.attempts.unwrap_or(0),
        claimed_callback = optional_graphql_string_field(
            "claimed_callback",
            invocation.claimed_callback.as_deref()
        ),
        plan = optional_graphql_string_field("action_plan", invocation.action_plan.as_deref()),
        journal = escape_graphql_string(journal),
        error = optional_graphql_string_field("error", invocation.error.as_deref()),
        claimed_at = optional_graphql_string_field("claimed_at", invocation.claimed_at.as_deref()),
    )
}

fn optional_graphql_string_field(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_else(|| format!("{name}: null,"))
}

pub async fn update_invocation(
    access: &gents_store::config_client::ConfigAccess,
    invocation: &CallbackInvocationDoc,
    expected_state: Option<&str>,
) -> Result<bool> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(invocation.agent_did.as_str()),
        "callback update actor mismatch"
    );
    let mutation = update_invocation_mutation(invocation, expected_state);
    let response = access
        .transact_write("callback.update_invocation", |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute_local_response(mutation).await })
        })
        .await?;
    Ok(
        gents_store::graphql::single_mutation_document(&response, "update_CallbackInvocation")?
            .is_some(),
    )
}

pub(super) fn create_callback_result_mutation(
    result: &CallbackResultDoc,
    agent_doc_id: &str,
    parent_linked: bool,
) -> String {
    let now = result
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    format!(
        r#"mutation {{
            create_CallbackResult(input: {{
                result_id: "{result_id}",
                invocation_id: "{invocation_id}",
                {binding_id}
                owner_node_did: "{owner}",
                agent_did: "{agent}",
                agent_doc_id: "{agent_doc_id}",
                parent_linked: {parent_linked},
                {workspace}
                {work_unit_id}
                {correlation}
                {route},
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#,
        result_id = escape_graphql_string(&result.result_id),
        invocation_id = escape_graphql_string(&result.invocation_id),
        binding_id = optional_graphql_string_field("binding_id", result.binding_id.as_deref()),
        owner = escape_graphql_string(&result.owner_node_did),
        agent = escape_graphql_string(&result.agent_did),
        agent_doc_id = escape_graphql_string(agent_doc_id),
        workspace = optional_graphql_string_field("workspace_id", result.workspace_id.as_deref()),
        work_unit_id =
            optional_graphql_string_field("work_unit_id", result.work_unit_id.as_deref()),
        correlation = optional_graphql_string_field(
            "caused_by_correlation",
            result.caused_by_correlation.as_deref()
        ),
        route = gents_session::workspace::handoff_route_fields(&result.route),
        created_at = escape_graphql_string(&now),
    )
}

pub async fn create_callback_result(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &gents_store::config_client::ConfigAccess,
    result: &CallbackResultDoc,
) -> Result<CallbackResultDoc> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(result.agent_did.as_str()),
        "callback result actor mismatch"
    );
    if let Some(existing) = load_callback_result(
        node,
        &result.invocation_id,
        &result.owner_node_did,
        &result.agent_did,
    )
    .await?
    {
        ensure_callback_parent(
            node,
            access,
            "CallbackResult",
            &existing.invocation_id,
            &existing.owner_node_did,
            &existing.agent_did,
        )
        .await?;
        return Ok(existing);
    }
    let stored = match access
        .transact_write("callback.create_result", |txn| {
            Box::pin(async move {
                let parent =
                    callback_agent_parent_in_txn(txn, &result.owner_node_did, &result.agent_did)
                        .await?;
                let response = txn
                    .execute(&create_callback_result_mutation(result, &parent, true))
                    .await?;
                stage_callback_child_parent(txn, "CallbackResult", &response, &parent).await?;
                Ok::<_, anyhow::Error>(response)
            })
        })
        .await
    {
        Ok(_) => load_callback_result(
            node,
            &result.invocation_id,
            &result.owner_node_did,
            &result.agent_did,
        )
        .await?
        .ok_or_else(|| anyhow!("created CallbackResult missing after write")),
        Err(error) => {
            if let Some(existing) = load_callback_result(
                node,
                &result.invocation_id,
                &result.owner_node_did,
                &result.agent_did,
            )
            .await?
            {
                ensure_callback_parent(
                    node,
                    access,
                    "CallbackResult",
                    &existing.invocation_id,
                    &existing.owner_node_did,
                    &existing.agent_did,
                )
                .await?;
                return Ok(existing);
            }
            Err(error).context("create_CallbackResult")
        }
    }?;
    Ok(stored)
}

pub async fn load_repository_placement(
    access: &gents_store::config_client::ConfigAccess,
    repository_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<RepositoryPlacementRef>> {
    anyhow::ensure!(
        !agent_did.is_empty() && access.actor_did().as_deref() == Some(agent_did),
        "callback workspace actor mismatch"
    );
    let row: Option<gents_documents::RepositoryPlacement> = access
        .transact_readonly("callback.read_repository_placement", |txn| {
            Box::pin(async move {
                gents_documents::read_desired_state_document_in_txn(
                    txn,
                    gents_store::collection::Collection::RepositoryPlacement,
                    owner,
                    repository_id,
                )
                .await?
                .map(|value| serde_json::from_value(value).map_err(anyhow::Error::from))
                .transpose()
            })
        })
        .await?;
    Ok(row.map(|row| RepositoryPlacementRef {
        repository_id: row.repository_id,
        owner_node_did: row.node_did,
        host_path: PathBuf::from(row.host_path),
        enabled: row.enabled,
    }))
}

pub async fn load_memory_workspace_docs(
    access: &gents_store::config_client::ConfigAccess,
    workspace_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<MemoryWorkspaceDocuments> {
    let mut docs = MemoryWorkspaceDocuments::default();
    if let Some(workspace) = load_isolated_workspace(access, workspace_id, owner, agent_did).await?
    {
        docs.write_isolated_workspace(workspace)?;
    }
    if let Some(placement) =
        load_workspace_placement(access, workspace_id, owner, agent_did).await?
    {
        docs.write_placement(placement)?;
    }
    Ok(docs)
}

pub(crate) use gents_session::workspace::flush_workspace_docs;

pub(crate) async fn load_isolated_workspace(
    access: &gents_store::config_client::ConfigAccess,
    workspace_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<IsolatedWorkspaceDoc>> {
    anyhow::ensure!(
        !agent_did.is_empty() && access.actor_did().as_deref() == Some(agent_did),
        "callback workspace actor mismatch"
    );
    use gents_protocol::document_query::{
        fields::isolated_workspace as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(Selection::owned(ISOLATED_WORKSPACE_FIELDS))
        .filter(
            Filter::eq(fields::WORKSPACE_ID, workspace_id)
                .and(Filter::eq(fields::OWNER_NODE_DID, owner)),
        )
        .limit(2)
        .render();
    let response = access.read(&rendered).await?;
    let mut found: Vec<IsolatedWorkspaceDoc> =
        serde_json::from_value(response["data"]["IsolatedWorkspace"].clone())?;
    anyhow::ensure!(
        found.len() <= 1,
        "ambiguous node-scoped callback/workspace document"
    );
    Ok(found.pop())
}

pub(crate) async fn load_workspace_placement(
    access: &gents_store::config_client::ConfigAccess,
    workspace_id: &str,
    owner: &str,
    agent_did: &str,
) -> Result<Option<WorkspacePlacementDoc>> {
    anyhow::ensure!(
        !agent_did.is_empty() && access.actor_did().as_deref() == Some(agent_did),
        "callback workspace actor mismatch"
    );
    use gents_protocol::document_query::{
        fields::workspace_placement as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(Selection::owned(PLACEMENT_FIELDS))
        .filter(
            Filter::eq(fields::WORKSPACE_ID, workspace_id)
                .and(Filter::eq(fields::OWNER_NODE_DID, owner)),
        )
        .limit(2)
        .render();
    let response = access.read(&rendered).await?;
    let mut found: Vec<WorkspacePlacementDoc> =
        serde_json::from_value(response["data"]["WorkspacePlacement"].clone())?;
    anyhow::ensure!(
        found.len() <= 1,
        "ambiguous node-scoped callback/workspace document"
    );
    Ok(found.pop())
}
