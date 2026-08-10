use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};

use super::controller::InferenceCallRecord;
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

pub(super) fn spawn_persistence<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    }
}

pub(super) fn completion_persistence_error(error: anyhow::Error) -> CompletionError {
    CompletionError::ProviderError(format!("persisting InferenceCall failed: {error:#}"))
}

fn extract_inference_call_doc_id(data: Option<&serde_json::Value>) -> Result<String> {
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
        .ok_or_else(|| anyhow::anyhow!("add_InferenceCall returned no _docID"))
}

pub(super) async fn persist_call_queued(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "queued", None, Some(&now), None, None, None);
    let resp =
        graphql_with_transaction_retry(node.as_ref(), &mutation, "persist queued InferenceCall")
            .await?;
    extract_inference_call_doc_id(resp.data.as_ref())
}

pub(super) async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, CompletionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp =
        graphql_with_transaction_retry(node.as_ref(), &mutation, "persist running InferenceCall")
            .await
            .map_err(completion_persistence_error)?;
    extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)
}

pub(super) async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_running_mutation(call, &now);
    graphql_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist existing running InferenceCall",
    )
    .await?;
    Ok(())
}

pub(super) async fn persist_terminal_call(
    node: Arc<EmbeddedNode>,
    call: InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
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
    graphql_with_transaction_retry(node.as_ref(), &mutation, "persist terminal InferenceCall")
        .await?;
    Ok(())
}

pub(super) async fn persist_existing_call_terminal(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_terminal_mutation(call, call_state, failure_reason, &now, usage);
    graphql_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist existing terminal InferenceCall",
    )
    .await?;
    Ok(())
}

fn add_call_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    queued_at: Option<&str>,
    started_at: Option<&str>,
    ended_at: Option<&str>,
    usage: Option<Usage>,
) -> String {
    let queued_at = optional_graphql_string("queued_at", queued_at);
    let started_at = optional_graphql_string("started_at", started_at);
    let ended_at = optional_graphql_string("ended_at", ended_at);
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "{runtime_instance_id}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                call_seq: {call_seq},
                backend_id: "{backend_id}",
                behavior_id: "{behavior_id}",
                agent_did: "{agent_did}",
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
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
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

fn upsert_call_running_mutation(call: &InferenceCallRecord, started_at: &str) -> String {
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    request_doc_id: "{request_doc_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "running",
                    queued_at: "{started_at}",
                    started_at: "{started_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                }},
                update: {{
                    call_state: "running",
                    started_at: "{started_at}"
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        request_doc_id = escape_graphql_string(&call.request_doc_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        started_at = escape_graphql_string(started_at),
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
    )
}

fn upsert_call_terminal_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    ended_at: &str,
    usage: Option<Usage>,
) -> String {
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    request_doc_id: "{request_doc_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "{call_state}",
                    {failure_reason}
                    queued_at: "{ended_at}",
                    ended_at: "{ended_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                    {prompt_tokens}
                    {completion_tokens}
                    {cached_input_tokens}
                }},
                update: {{
                    call_state: "{call_state}",
                    {failure_reason}
                    ended_at: "{ended_at}"
                    {prompt_tokens}
                    {completion_tokens}
                    {cached_input_tokens}
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        request_doc_id = escape_graphql_string(&call.request_doc_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        ended_at = escape_graphql_string(ended_at),
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

fn usage_fields(usage: Option<Usage>) -> (String, String, String) {
    match usage {
        Some(usage) => {
            let (prompt_tokens, completion_tokens, cached_input_tokens) =
                crate::provider_usage::persisted_usage_counts(usage);
            (
                format!("prompt_tokens: {prompt_tokens},"),
                format!("completion_tokens: {completion_tokens},"),
                format!("cached_input_tokens: {cached_input_tokens},"),
            )
        }
        None => (String::new(), String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::client::CallKind;

    fn call() -> InferenceCallRecord {
        InferenceCallRecord {
            call_id: "call-1".to_string(),
            runtime_instance_id: "runtime-1".to_string(),
            request_id: "request-logical".to_string(),
            request_doc_id: "request-doc-physical".to_string(),
            call_seq: 1,
            backend_id: "backend-1".to_string(),
            behavior_id: "behavior-1".to_string(),
            agent_did: "did:test:agent".to_string(),
            call_kind: CallKind::Inference,
            attempt: 1,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "fingerprint-1".to_string(),
        }
    }

    #[test]
    fn every_inference_call_create_arm_persists_the_physical_request_edge() {
        let call = call();
        let mutations = [
            add_call_mutation(
                &call,
                "queued",
                None,
                Some("2026-08-09T00:00:00Z"),
                None,
                None,
                None,
            ),
            upsert_call_running_mutation(&call, "2026-08-09T00:00:01Z"),
            upsert_call_terminal_mutation(&call, "completed", None, "2026-08-09T00:00:02Z", None),
        ];

        for mutation in mutations {
            assert!(
                mutation.contains(r#"request_id: "request-logical""#),
                "logical correlation id missing from mutation: {mutation}"
            );
            assert!(
                mutation.contains(r#"request_doc_id: "request-doc-physical""#),
                "physical request edge missing from mutation: {mutation}"
            );
        }
    }
}
