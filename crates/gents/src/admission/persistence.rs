use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};

use super::controller::InferenceCallRecord;
use crate::graphql::{escape_graphql_string, graphql_mutation_with_transaction_retry};

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
    let resp = graphql_mutation_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist queued InferenceCall",
    )
    .await?;
    extract_inference_call_doc_id(resp.data.as_ref())
}

pub(super) async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, CompletionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp = graphql_mutation_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist running InferenceCall",
    )
    .await
    .map_err(completion_persistence_error)?;
    extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)
}

pub(super) async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let call_id = escape_graphql_string(&call.call_id);
    let started_at = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{ update_InferenceCall(
            filter: {{ call_id: {{ _eq: "{call_id}" }}, call_state: {{ _eq: "queued" }} }},
            input: {{ call_state: "running", started_at: "{started_at}" }}
        ) {{ _docID }} }}"#
    );
    let response = graphql_mutation_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist existing running InferenceCall",
    )
    .await?;
    anyhow::ensure!(
        !crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty(),
        "InferenceCall {} is no longer queued or is missing",
        call.call_id
    );
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
    graphql_mutation_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist terminal InferenceCall",
    )
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
    // Proofs/InferenceCall/Persistence.lean: only an existing legal source
    // may install the first terminal outcome; later observations are usage only.
    let live_source = match call_state {
        "completed" | "failed" => r#"{ _eq: "running" }"#,
        "cancelled" => r#"{ _in: ["queued", "running"] }"#,
        _ => anyhow::bail!("invalid terminal InferenceCall state: {call_state}"),
    };
    let call_id = escape_graphql_string(&call.call_id);
    let ended_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) = usage_fields(usage);
    let mutation = format!(
        r#"mutation {{ update_InferenceCall(
            filter: {{ call_id: {{ _eq: "{call_id}" }}, call_state: {live_source} }},
            input: {{ call_state: "{call_state}", {failure_reason} ended_at: "{ended_at}",
                {prompt_tokens} {completion_tokens} {cached_input_tokens} }}
        ) {{ _docID }} }}"#
    );
    let response = graphql_mutation_with_transaction_retry(
        node.as_ref(),
        &mutation,
        "persist existing terminal InferenceCall",
    )
    .await?;
    if !crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty() {
        return Ok(());
    }

    let terminal_filter = format!(
        r#"call_id: {{ _eq: "{call_id}" }}, call_state: {{ _in: ["completed", "failed", "cancelled"] }}"#
    );
    let exists = if usage.is_some() {
        // Recovery may win while a provider response is in flight. Record its
        // observed usage without changing the winning outcome, reason or stamp.
        let mutation = format!(
            r#"mutation {{ update_InferenceCall(
                filter: {{ {terminal_filter} }},
                input: {{ {prompt_tokens} {completion_tokens} {cached_input_tokens} }}
            ) {{ _docID }} }}"#
        );
        let response = graphql_mutation_with_transaction_retry(
            node.as_ref(),
            &mutation,
            "persist late InferenceCall usage",
        )
        .await?;
        !crate::graphql::rows::<serde_json::Value>(&response, "update_InferenceCall")?.is_empty()
    } else {
        // An already terminal row is an idempotent success. A missing row or
        // illegal live source is a durability failure, never an implicit insert.
        let query = format!(r#"{{ InferenceCall(filter: {{ {terminal_filter} }}) {{ _docID }} }}"#);
        let response = crate::graphql::graphql_with_transaction_retry(
            node.as_ref(),
            &query,
            "confirm terminal InferenceCall",
        )
        .await?;
        !crate::graphql::rows::<serde_json::Value>(&response, "InferenceCall")?.is_empty()
    };
    anyhow::ensure!(
        exists,
        "InferenceCall {} is missing or cannot transition to {call_state}",
        call.call_id
    );
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

fn optional_graphql_string(field: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{field}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

fn usage_fields(usage: Option<Usage>) -> (String, String, String) {
    match usage {
        Some(usage) => (
            format!("prompt_tokens: {},", usage.input_tokens),
            format!("completion_tokens: {},", usage.output_tokens),
            format!("cached_input_tokens: {},", usage.cached_input_tokens),
        ),
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
            add_call_mutation(
                &call,
                "running",
                None,
                Some("2026-08-09T00:00:01Z"),
                Some("2026-08-09T00:00:01Z"),
                None,
                None,
            ),
            add_call_mutation(
                &call,
                "failed",
                Some("QueueFull"),
                Some("2026-08-09T00:00:02Z"),
                None,
                Some("2026-08-09T00:00:02Z"),
                None,
            ),
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

    #[test]
    fn usage_columns_preserve_provider_components_verbatim() {
        let fields = usage_fields(Some(Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 200,
            cached_input_tokens: 40,
            cache_creation_input_tokens: 10,
        }));

        assert_eq!(fields.0, "prompt_tokens: 100,");
        assert_eq!(fields.1, "completion_tokens: 50,");
        assert_eq!(fields.2, "cached_input_tokens: 40,");
    }
}
