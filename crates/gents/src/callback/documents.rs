use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::Value;

use crate::graphql::{escape_graphql_string, first_row, rows};

async fn committed_mutation(
    node: &EmbeddedNode,
    operation: &'static str,
    mutation: &str,
) -> Result<defra_node::QueryResponse> {
    crate::config_client::ConfigAccess::write_local_response(node, operation, mutation).await
}
use crate::workspace::{
    isolated_workspace_upsert_mutation, workspace_placement_upsert_mutation, IsolatedWorkspaceDoc,
    MemoryWorkspaceDocuments, RepositoryPlacementRef, WorkspaceDocuments, WorkspacePlacementDoc,
};

/// Crash-window bound for succeeded-without-result repair. Happy-path
/// succeeded rows are excluded by a batched CallbackResult lookup, not by
/// probing every historical invocation.
pub(crate) const SUCCEEDED_REPAIR_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const SUCCEEDED_REPAIR_LIMIT: u32 = 256;

const INVOCATION_FIELDS: &str = "invocation_id owner_agent_did callback_id origin input idempotency_key lifecycle_state attempts action_plan action_journal error claimed_at created_at";
const RESULT_FIELDS: &str = "result_id invocation_id owner_agent_did workspace_id work_unit_id caused_by_correlation created_at";

const ISOLATED_WORKSPACE_FIELDS: &str = r#"
    workspace_id
    work_unit_id
    repository_id
    base_sha
    branch
    creation_policy
    adapter
    owner_agent_did
    writer_principal
    integrator_principal
    instruction_manifest
    path_capability
    seal_hash
    lifecycle_state
    caused_by_invocation_id
    caused_by_correlation
"#;

const PLACEMENT_FIELDS: &str = r#"
    workspace_id
    owner_agent_did
    host_path
    repository_placement_id
    adapter
    adapter_version
    dirty_base
    dirty_base_summary
    provisioning_state
    observed_tree_hash
"#;

// Canonical event callback configuration is shared with packs.
pub use crate::document_config::{
    CallbackBinding as CallbackBindingDoc, CallbackModule as CallbackModuleDoc,
};

impl CallbackBindingDoc {
    pub fn projected_fields(&self) -> Result<Vec<String>> {
        validate_callback_binding(self)?;
        Ok(self.input_fields.clone())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackInvocationDoc {
    /// Immutable projected planner input captured before dispatch. Events use an
    /// object and groups an ordered array of projected objects.
    /// Reuse the existing planner input and action journal; retries never reread
    /// changed source rows or recompute a persisted action plan.
    #[serde(deserialize_with = "deserialize_callback_input")]
    pub input: serde_json::Value,
    pub invocation_id: String,
    /// Principal whose runtime owns execution and recovery.
    pub owner_agent_did: String,
    pub callback_id: String,
    pub origin: crate::document_config::CallbackInvocationOrigin,
    pub idempotency_key: String,
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
    /// Principal whose runtime owns execution and recovery.
    pub owner_agent_did: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub work_unit_id: Option<String>,
    #[serde(default)]
    pub caused_by_correlation: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CallbackResultInvocationRow {
    pub invocation_id: String,
}

pub fn idempotency_key(binding_id: &str, source_doc_id: &str, source_version: &str) -> String {
    format!("{binding_id}:{source_doc_id}:{source_version}")
}

pub fn parse_string_list(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) {
        return items
            .into_iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect();
    }
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// Bindings that list secret-bearing source fields fail closed at apply/load.
pub fn validate_callback_binding(binding: &CallbackBindingDoc) -> Result<()> {
    for (name, value) in [
        ("binding_id", &binding.binding_id),
        ("agent_did", &binding.agent_did),
        ("callback_id", &binding.callback_id),
        ("event_source_id", &binding.event_source_id),
    ] {
        anyhow::ensure!(
            !value.trim().is_empty(),
            "CallbackBinding {name} must be non-empty"
        );
    }
    let mut unique = HashSet::new();
    for field in &binding.input_fields {
        crate::graphql::validate_graphql_name(field)?;
        anyhow::ensure!(
            unique.insert(field),
            "duplicate callback input field {field}"
        );
        anyhow::ensure!(
            !crate::toolset::is_secret_env_name(field),
            "CallbackBinding {} source field `{field}` is secret-bearing",
            binding.binding_id
        );
    }
    Ok(())
}

pub fn reject_secret_bearing_callback_fields(
    binding_id: &str,
    filter: Option<&str>,
    source_fields: Option<&str>,
) -> Result<()> {
    if let Some(filter) = filter.map(str::trim).filter(|filter| !filter.is_empty()) {
        crate::graphql::validate_graphql_filter_fragment(filter)?;
        if let Some(field) = secret_field_in_filter(filter) {
            anyhow::bail!("CallbackBinding {binding_id} filter field `{field}` is secret-bearing");
        }
    }
    for field in parse_string_list(source_fields) {
        crate::graphql::validate_graphql_name(&field)?;
        if crate::toolset::is_secret_env_name(&field) {
            anyhow::bail!("CallbackBinding {binding_id} source field `{field}` is secret-bearing");
        }
    }
    Ok(())
}

fn secret_field_in_filter(filter: &str) -> Option<String> {
    let stripped = strip_graphql_string_literals(filter);
    let mut ident = String::new();
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
            continue;
        }
        if let Some(secret) = secret_ident(&ident) {
            return Some(secret);
        }
        ident.clear();
    }
    secret_ident(&ident)
}

fn secret_ident(ident: &str) -> Option<String> {
    if ident.is_empty() || ident.starts_with('_') {
        return None;
    }
    crate::toolset::is_secret_env_name(ident).then(|| ident.to_string())
}

fn strip_graphql_string_literals(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            out.push(ch);
            continue;
        }
        while let Some(next) = chars.next() {
            if next == '\\' {
                let _ = chars.next();
                continue;
            }
            if next == '"' {
                break;
            }
        }
    }
    out
}

pub fn strip_secret_fields(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| !crate::toolset::is_secret_env_name(key))
                .map(|(key, child)| (key, strip_secret_fields(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_secret_fields).collect()),
        other => other,
    }
}

async fn load_config<T: serde::de::DeserializeOwned + Send>(
    node: &EmbeddedNode,
    collection: crate::Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>> {
    crate::config_client::ConfigAccess::transact_local(node, None, "callback.read_config", |txn| {
        Box::pin(async move {
            crate::config_client::read_desired_state_record_in_txn(txn, collection, owner, id)
                .await?
                .map(|(_, value)| serde_json::from_value(value).map_err(Into::into))
                .transpose()
        })
    })
    .await
}

pub async fn list_enabled_bindings(
    node: &EmbeddedNode,
    owner: &str,
) -> Result<Vec<CallbackBindingDoc>> {
    let (fields, _) =
        crate::config_client::config_projection(crate::Collection::CallbackBinding, None)?;
    let query = format!(
        "{{ CallbackBinding(filter: {{ agent_did: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
        escape_graphql_string(owner),
        fields.join(" ")
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "query CallbackBinding failed: {:?}",
        response.errors
    );
    let mut bindings: Vec<CallbackBindingDoc> = rows(&response, "CallbackBinding")?;
    let mut ids = HashSet::new();
    for binding in &bindings {
        anyhow::ensure!(
            binding.agent_did == owner && ids.insert(&binding.binding_id),
            "ambiguous or foreign CallbackBinding"
        );
        validate_callback_binding(binding)?;
    }
    bindings.retain(|binding| binding.enabled);
    bindings.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
    Ok(bindings)
}

pub async fn load_callback_module(
    node: &EmbeddedNode,
    module_id: &str,
    owner: &str,
) -> Result<Option<CallbackModuleDoc>> {
    load_config(node, crate::Collection::CallbackModule, owner, module_id).await
}
pub async fn load_callback(
    node: &EmbeddedNode,
    callback_id: &str,
    owner: &str,
) -> Result<Option<crate::document_config::Callback>> {
    load_config(node, crate::Collection::Callback, owner, callback_id).await
}
pub async fn load_event_source(
    node: &EmbeddedNode,
    source_id: &str,
    owner: &str,
) -> Result<Option<crate::document_config::EventSource>> {
    load_config(node, crate::Collection::EventSource, owner, source_id).await
}

pub async fn load_trusted_callback_signers(node: &EmbeddedNode) -> Result<BTreeSet<String>> {
    let query = r#"{
        AgentPrincipal(filter: { enabled: { _eq: true } }) {
            agent_did
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query trusted AgentPrincipal signers failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<Value> = rows(&response, "AgentPrincipal")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.get("agent_did")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|did| !did.is_empty())
                .map(str::to_string)
        })
        .collect())
}

pub async fn load_invocation(
    node: &EmbeddedNode,
    invocation_id: &str,
    owner: &str,
) -> Result<Option<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ invocation_id: {{ _eq: "{id}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        id = escape_graphql_string(invocation_id),
        owner = escape_graphql_string(owner),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query CallbackInvocation {invocation_id} failed: {:?}",
            response.errors
        );
    }
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackInvocation").map(|rows| rows.len())? <= 1,
        "ambiguous principal-scoped callback/workspace document"
    );
    first_row(&response, "CallbackInvocation")
}

pub async fn load_invocation_by_key(
    node: &EmbeddedNode,
    key: &str,
    owner: &str,
) -> Result<Option<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ idempotency_key: {{ _eq: "{key}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        key = escape_graphql_string(key),
        owner = escape_graphql_string(owner),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query CallbackInvocation by idempotency_key failed: {:?}",
            response.errors
        );
    }
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackInvocation").map(|rows| rows.len())? <= 1,
        "ambiguous principal-scoped callback/workspace document"
    );
    first_row(&response, "CallbackInvocation")
}

pub async fn list_recoverable_invocations(
    node: &EmbeddedNode,
    owner_agent_did: &str,
) -> Result<Vec<CallbackInvocationDoc>> {
    let mut invocations = query_owner_invocations(
        node,
        owner_agent_did,
        r#"["pending", "claimed", "running"]"#,
    )
    .await?;
    invocations.extend(list_recent_succeeded_missing_result(node, owner_agent_did).await?);
    Ok(invocations)
}

async fn list_recent_succeeded_missing_result(
    node: &EmbeddedNode,
    owner_agent_did: &str,
) -> Result<Vec<CallbackInvocationDoc>> {
    let cutoff = succeeded_repair_cutoff(chrono::Utc::now());
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{
                    owner_agent_did: {{ _eq: "{owner}" }},
                    lifecycle_state: {{ _eq: "succeeded" }},
                    created_at: {{ _ge: "{cutoff}" }}
                }},
                order: {{ created_at: DESC }},
                limit: {limit}
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        owner = escape_graphql_string(owner_agent_did),
        cutoff = escape_graphql_string(&cutoff),
        limit = SUCCEEDED_REPAIR_LIMIT,
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query recent succeeded CallbackInvocation failed: {:?}",
            response.errors
        );
    }
    let succeeded: Vec<CallbackInvocationDoc> = rows(&response, "CallbackInvocation")?;
    if succeeded.is_empty() {
        return Ok(Vec::new());
    }
    let results = load_callback_results_for_invocations(
        node,
        succeeded.iter().map(|row| row.invocation_id.as_str()),
        owner_agent_did,
    )
    .await?;
    Ok(succeeded_missing_result(succeeded, &results))
}

pub(crate) fn succeeded_repair_cutoff(now: chrono::DateTime<chrono::Utc>) -> String {
    let start = now
        - chrono::Duration::from_std(SUCCEEDED_REPAIR_WINDOW)
            .unwrap_or_else(|_| chrono::Duration::hours(24));
    start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn succeeded_missing_result(
    succeeded: Vec<CallbackInvocationDoc>,
    result_invocation_ids: &HashSet<String>,
) -> Vec<CallbackInvocationDoc> {
    succeeded
        .into_iter()
        .filter(|row| !result_invocation_ids.contains(&row.invocation_id))
        .collect()
}

async fn load_callback_results_for_invocations(
    node: &EmbeddedNode,
    invocation_ids: impl IntoIterator<Item = &str>,
    owner: &str,
) -> Result<HashSet<String>> {
    let ids: Vec<String> = invocation_ids
        .into_iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect();
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let query = format!(
        r#"{{
            CallbackResult(
                filter: {{ invocation_id: {{ _in: [{ids}] }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: {limit}
            ) {{ invocation_id }}
        }}"#,
        ids = ids.join(", "),
        owner = escape_graphql_string(owner),
        limit = SUCCEEDED_REPAIR_LIMIT,
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query CallbackResult batch for succeeded repair failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<CallbackResultInvocationRow> = rows(&response, "CallbackResult")?;
    Ok(rows.into_iter().map(|row| row.invocation_id).collect())
}

async fn query_owner_invocations(
    node: &EmbeddedNode,
    owner_agent_did: &str,
    states: &str,
) -> Result<Vec<CallbackInvocationDoc>> {
    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{
                    owner_agent_did: {{ _eq: "{owner}" }},
                    lifecycle_state: {{ _in: {states} }}
                }},
                order: {{ created_at: ASC }}
            ) {{ {INVOCATION_FIELDS} }}
        }}"#,
        owner = escape_graphql_string(owner_agent_did),
        states = states,
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query CallbackInvocation states {states} failed: {:?}",
            response.errors
        );
    }
    rows(&response, "CallbackInvocation")
}

pub async fn load_callback_result(
    node: &EmbeddedNode,
    invocation_id: &str,
    owner: &str,
) -> Result<Option<CallbackResultDoc>> {
    let query = format!(
        r#"{{
            CallbackResult(
                filter: {{ invocation_id: {{ _eq: "{id}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {RESULT_FIELDS} }}
        }}"#,
        id = escape_graphql_string(invocation_id),
        owner = escape_graphql_string(owner),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query CallbackResult for {invocation_id} failed: {:?}",
            response.errors
        );
    }
    anyhow::ensure!(
        rows::<Value>(&response, "CallbackResult").map(|rows| rows.len())? <= 1,
        "ambiguous principal-scoped callback/workspace document"
    );
    first_row(&response, "CallbackResult")
}

pub async fn create_pending_invocation(
    node: &EmbeddedNode,
    invocation: &CallbackInvocationDoc,
) -> Result<CallbackInvocationDoc> {
    if let Some(existing) = load_invocation_by_key(
        node,
        &invocation.idempotency_key,
        &invocation.owner_agent_did,
    )
    .await?
    {
        return Ok(existing);
    }
    let now = invocation
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let input = serde_json::json!({
        "invocation_id": invocation.invocation_id, "owner_agent_did": invocation.owner_agent_did,
        "callback_id": invocation.callback_id, "origin": invocation.origin,
        "input": callback_input_for_storage(&invocation.input),
        "idempotency_key": invocation.idempotency_key, "lifecycle_state": "pending", "attempts": 0,
        "action_plan": "", "action_journal": "[]", "error": "", "claimed_at": "", "created_at": now
    });
    let variables = serde_json::json!({"input": input});
    let mutation = "mutation($input: CallbackInvocationMutationInputArg!) { create_CallbackInvocation(input: $input) { _docID } }";
    let written = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "callback.create_invocation",
        |txn| {
            let variables = &variables;
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        },
    )
    .await;
    match written {
        Ok(_) => load_invocation_by_key(
            node,
            &invocation.idempotency_key,
            &invocation.owner_agent_did,
        )
        .await?
        .ok_or_else(|| anyhow!("created CallbackInvocation missing after write")),
        Err(error) => {
            if let Some(existing) = load_invocation_by_key(
                node,
                &invocation.idempotency_key,
                &invocation.owner_agent_did,
            )
            .await?
            {
                return Ok(existing);
            }
            Err(error).context("create_CallbackInvocation")
        }
    }
}

pub async fn update_invocation(
    node: &EmbeddedNode,
    invocation: &CallbackInvocationDoc,
    expected_state: Option<&str>,
) -> Result<bool> {
    let state_filter = expected_state
        .map(|state| {
            format!(
                r#", lifecycle_state: {{ _eq: "{}" }}"#,
                escape_graphql_string(state)
            )
        })
        .unwrap_or_default();
    let journal = invocation.action_journal.as_deref().unwrap_or("[]");
    let plan = invocation.action_plan.as_deref().unwrap_or("");
    let error = invocation.error.as_deref().unwrap_or("");
    let claimed_at = invocation.claimed_at.as_deref().unwrap_or("");
    let mutation = format!(
        r#"mutation {{
            update_CallbackInvocation(
                filter: {{
                    invocation_id: {{ _eq: "{id}" }},
                    owner_agent_did: {{ _eq: "{owner}" }}
                    {state_filter}
                }},
                input: {{
                    lifecycle_state: "{state}",
                    attempts: {attempts},
                    action_plan: "{plan}",
                    action_journal: "{journal}",
                    error: "{error}",
                    claimed_at: "{claimed_at}"
                }}
            ) {{ _docID }}
        }}"#,
        id = escape_graphql_string(&invocation.invocation_id),
        owner = escape_graphql_string(&invocation.owner_agent_did),
        state = escape_graphql_string(&invocation.lifecycle_state),
        attempts = invocation.attempts.unwrap_or(0),
        plan = escape_graphql_string(plan),
        journal = escape_graphql_string(journal),
        error = escape_graphql_string(error),
        claimed_at = escape_graphql_string(claimed_at),
    );
    let response = committed_mutation(node, "callback.update_invocation", &mutation).await?;
    Ok(crate::graphql::single_mutation_document(&response, "update_CallbackInvocation")?.is_some())
}

pub async fn create_callback_result(
    node: &EmbeddedNode,
    result: &CallbackResultDoc,
) -> Result<CallbackResultDoc> {
    if let Some(existing) =
        load_callback_result(node, &result.invocation_id, &result.owner_agent_did).await?
    {
        return Ok(existing);
    }
    let now = result
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let workspace = result.workspace_id.as_deref().unwrap_or("");
    let work_unit_id = result.work_unit_id.as_deref().unwrap_or("");
    let correlation = result.caused_by_correlation.as_deref().unwrap_or("");
    let mutation = format!(
        r#"mutation {{
            create_CallbackResult(input: {{
                result_id: "{result_id}",
                invocation_id: "{invocation_id}",
                owner_agent_did: "{owner}",
                workspace_id: "{workspace}",
                work_unit_id: "{work_unit_id}",
                caused_by_correlation: "{correlation}",
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#,
        result_id = escape_graphql_string(&result.result_id),
        invocation_id = escape_graphql_string(&result.invocation_id),
        owner = escape_graphql_string(&result.owner_agent_did),
        workspace = escape_graphql_string(workspace),
        work_unit_id = escape_graphql_string(work_unit_id),
        correlation = escape_graphql_string(correlation),
        created_at = escape_graphql_string(&now),
    );
    match committed_mutation(node, "callback.create_result", &mutation).await {
        Ok(_) => load_callback_result(node, &result.invocation_id, &result.owner_agent_did)
            .await?
            .ok_or_else(|| anyhow!("created CallbackResult missing after write")),
        Err(error) => {
            if let Some(existing) =
                load_callback_result(node, &result.invocation_id, &result.owner_agent_did).await?
            {
                return Ok(existing);
            }
            Err(error).context("create_CallbackResult")
        }
    }
}

pub async fn load_repository_placement(
    node: &EmbeddedNode,
    repository_id: &str,
    owner: &str,
) -> Result<Option<RepositoryPlacementRef>> {
    let row: Option<crate::document_config::RepositoryPlacement> = load_config(
        node,
        crate::Collection::RepositoryPlacement,
        owner,
        repository_id,
    )
    .await?;
    Ok(row.map(|row| RepositoryPlacementRef {
        repository_id: row.repository_id,
        owner_agent_did: row.agent_did,
        host_path: PathBuf::from(row.host_path),
        enabled: row.enabled,
    }))
}

pub async fn load_memory_workspace_docs(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner: &str,
) -> Result<MemoryWorkspaceDocuments> {
    let mut docs = MemoryWorkspaceDocuments::default();
    if let Some(workspace) = load_isolated_workspace(node, workspace_id, owner).await? {
        docs.write_isolated_workspace(workspace)?;
    }
    if let Some(placement) = load_workspace_placement(node, workspace_id, owner).await? {
        docs.write_placement(placement)?;
    }
    Ok(docs)
}

pub async fn flush_workspace_docs(
    node: &EmbeddedNode,
    docs: &MemoryWorkspaceDocuments,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for workspace in docs.workspaces.values() {
        let mutation = isolated_workspace_upsert_mutation(workspace);
        committed_mutation(node, "callback.upsert_isolated_workspace", &mutation)
            .await
            .with_context(|| format!("persist IsolatedWorkspace {}", workspace.workspace_id))?;
    }
    for placement in docs.placements.values() {
        let mutation = workspace_placement_upsert_mutation(placement, &now);
        committed_mutation(node, "callback.upsert_workspace_placement", &mutation)
            .await
            .with_context(|| format!("persist WorkspacePlacement {}", placement.workspace_id))?;
    }
    Ok(())
}

pub(crate) async fn load_isolated_workspace(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner: &str,
) -> Result<Option<IsolatedWorkspaceDoc>> {
    let query = format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {ISOLATED_WORKSPACE_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(owner),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query IsolatedWorkspace {workspace_id} failed: {:?}",
            response.errors
        );
    }
    anyhow::ensure!(
        rows::<Value>(&response, "IsolatedWorkspace").map(|rows| rows.len())? <= 1,
        "ambiguous principal-scoped callback/workspace document"
    );
    first_row(&response, "IsolatedWorkspace")
}

pub(crate) async fn load_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner: &str,
) -> Result<Option<WorkspacePlacementDoc>> {
    let query = format!(
        r#"{{
            WorkspacePlacement(
                filter: {{ workspace_id: {{ _eq: "{id}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{ {PLACEMENT_FIELDS} }}
        }}"#,
        id = escape_graphql_string(workspace_id),
        owner = escape_graphql_string(owner),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query WorkspacePlacement {workspace_id} failed: {:?}",
            response.errors
        );
    }
    anyhow::ensure!(
        rows::<Value>(&response, "WorkspacePlacement").map(|rows| rows.len())? <= 1,
        "ambiguous principal-scoped callback/workspace document"
    );
    first_row(&response, "WorkspacePlacement")
}
