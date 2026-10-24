use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_loop::rig_compat::CachedInputTokensObservation;

use super::client::AdmissionError;
use super::controller::InferenceCallRecord;
use crate::graphql::escape_graphql_string;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PersistenceError {
    #[error(transparent)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error("add_InferenceCall returned no _docID")]
    MissingInferenceCallDocId,
    #[error("InferenceCall {call_id} is no longer queued or is missing")]
    CallNotQueued { call_id: String },
    #[error("invalid terminal InferenceCall state: {call_state}")]
    InvalidTerminalState { call_state: String },
    #[error("InferenceCall {call_id} is missing or cannot transition to {call_state}")]
    CallNotTransitionable { call_id: String, call_state: String },
}

type Result<T, E = PersistenceError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderCallUsage {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cached_input_tokens: u64,
}

pub(super) fn completion_persistence_error<E>(error: E) -> AdmissionError
where
    E: std::error::Error,
{
    AdmissionError(format!(
        "persisting InferenceCall failed: {}",
        ErrorChain(&error)
    ))
}

fn extract_inference_call_doc_id(
    data: Option<&serde_json::Value>,
) -> Result<String, PersistenceError> {
    data.and_then(|data| data.get("add_InferenceCall"))
        .and_then(|value| {
            value
                .get("_docID")
                .and_then(|doc_id| doc_id.as_str())
                .or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                        .and_then(|doc_id| doc_id.as_str())
                })
        })
        .map(ToOwned::to_owned)
        .ok_or(PersistenceError::MissingInferenceCallDocId)
}

pub(super) async fn persist_call_queued(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "queued", None, Some(&now), None, None, None);
    let resp = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "admission.persist_call_queued",
        &mutation,
    )
    .await?;
    extract_inference_call_doc_id(resp.data.as_ref())
}

pub(super) async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, AdmissionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "admission.persist_call_started",
        &mutation,
    )
    .await
    .map_err(completion_persistence_error)?;
    extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)
}

pub(super) async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
    doc_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let call_id = escape_graphql_string(&call.call_id);
    let doc_id = escape_graphql_string(doc_id);
    let started_at = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{ update_InferenceCall(
            docID: "{doc_id}",
            filter: {{ call_id: {{ _eq: "{call_id}" }}, call_state: {{ _eq: "queued" }} }},
            input: {{ call_state: "running", started_at: "{started_at}" }}
        ) {{ _docID }} }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "admission.persist_existing_call_running",
        &mutation,
    )
    .await?;
    if crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty() {
        return Err(PersistenceError::CallNotQueued {
            call_id: call.call_id.clone(),
        });
    }
    Ok(())
}

pub(super) async fn persist_terminal_call(
    node: Arc<EmbeddedNode>,
    call: InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<ProviderCallUsage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(
        &call,
        call_state,
        failure_reason,
        Some(&now),
        None,
        Some(&now),
        usage,
    );
    crate::config_client::ConfigAccess::write_local_response(
        &node,
        "admission.persist_terminal_call",
        &mutation,
    )
    .await?;
    Ok(())
}

pub(super) async fn persist_existing_call_terminal(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
    doc_id: &str,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<ProviderCallUsage>,
    cache_observation: CachedInputTokensObservation,
) -> Result<()> {
    // Proofs/InferenceCall/Persistence.lean: only an existing legal source
    // may install the first terminal outcome; later observations are usage only.
    let live_source = match call_state {
        "completed" | "failed" => r#"{ _eq: "running" }"#,
        "cancelled" => r#"{ _in: ["queued", "running"] }"#,
        _ => {
            return Err(PersistenceError::InvalidTerminalState {
                call_state: call_state.to_owned(),
            })
        }
    };
    let call_id = escape_graphql_string(&call.call_id);
    let doc_id = escape_graphql_string(doc_id);
    let ended_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) =
        usage_fields(usage, cache_observation);
    let mutation = format!(
        r#"mutation {{ update_InferenceCall(
            docID: "{doc_id}",
            filter: {{ call_id: {{ _eq: "{call_id}" }}, call_state: {live_source} }},
            input: {{ call_state: "{call_state}", {failure_reason} ended_at: "{ended_at}",
                {prompt_tokens} {completion_tokens} {cached_input_tokens} }}
        ) {{ _docID }} }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        &node,
        "admission.persist_existing_call_terminal",
        &mutation,
    )
    .await?;
    if !crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty() {
        return Ok(());
    }

    let terminal_filter = format!(
        r#"_docID: {{ _eq: "{doc_id}" }}, call_id: {{ _eq: "{call_id}" }}, call_state: {{ _in: ["completed", "failed", "cancelled"] }}"#
    );
    let exists = if usage.is_some() {
        let mutation = format!(
            r#"mutation {{ update_InferenceCall(
                docID: "{doc_id}",
                filter: {{ {terminal_filter} }},
                input: {{ {prompt_tokens} {completion_tokens} {cached_input_tokens} }}
            ) {{ _docID }} }}"#
        );
        let response = crate::config_client::ConfigAccess::write_local_response(
            &node,
            "admission.persist_late_call_usage",
            &mutation,
        )
        .await?;
        !crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty()
    } else {
        let query = format!(r#"{{ InferenceCall(filter: {{ {terminal_filter} }}) {{ _docID }} }}"#);
        let response = crate::graphql::graphql_with_transaction_retry(
            node.as_ref(),
            &query,
            "confirm terminal InferenceCall",
        )
        .await?;
        !crate::graphql::rows::<serde_json::Value>(&response, "InferenceCall")?.is_empty()
    };
    if !exists {
        return Err(PersistenceError::CallNotTransitionable {
            call_id: call.call_id.clone(),
            call_state: call_state.to_owned(),
        });
    }
    Ok(())
}

fn add_call_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    queued_at: Option<&str>,
    started_at: Option<&str>,
    ended_at: Option<&str>,
    usage: Option<ProviderCallUsage>,
) -> String {
    let queued_at = optional_graphql_string("queued_at", queued_at);
    let started_at = optional_graphql_string("started_at", started_at);
    let ended_at = optional_graphql_string("ended_at", ended_at);
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) =
        usage_fields(usage, CachedInputTokensObservation::NotAvailable);
    format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "{runtime_instance_id}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                call_seq: {call_seq},
                backend_id: "{backend_id}",
                agent_id: "{agent_id}",
                node_did: "{node_did}",
                call_kind: "{call_kind}",
                attempt: {attempt},
                call_state: "{call_state}",
                {failure_reason}
                {queued_at}
                {started_at}
                {ended_at}
                priority: 0,
                queue_depth_at_enqueue: {queue_depth_at_enqueue},
                controller_generation: {controller_generation},
                backend_config_fingerprint: "{backend_config_fingerprint}"
                {prompt_tokens}
                {completion_tokens}
                {cached_input_tokens}
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        request_doc_id = escape_graphql_string(&call.request_doc_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        agent_id = escape_graphql_string(&call.agent_id),
        node_did = escape_graphql_string(&call.node_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        queued_at = queued_at,
        started_at = started_at,
        ended_at = ended_at,
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
        cached_input_tokens = cached_input_tokens,
    )
}

fn optional_graphql_string(field: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{field}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

fn usage_fields(
    usage: Option<ProviderCallUsage>,
    cache_observation: CachedInputTokensObservation,
) -> (String, String, String) {
    match usage {
        Some(usage) => {
            let cached_input_tokens = match cache_observation {
                CachedInputTokensObservation::NotAvailable => Some(usage.cached_input_tokens),
                CachedInputTokensObservation::Unreported => None,
                CachedInputTokensObservation::Reported(tokens) => Some(tokens),
            };
            (
                format!("prompt_tokens: {},", usage.input_tokens),
                format!("completion_tokens: {},", usage.output_tokens),
                cached_input_tokens
                    .map(|tokens| format!("cached_input_tokens: {tokens},"))
                    .unwrap_or_default(),
            )
        }
        None => (String::new(), String::new(), String::new()),
    }
}
