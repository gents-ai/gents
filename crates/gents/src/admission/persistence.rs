use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};

use super::controller::InferenceCallRecord;
use crate::graphql::escape_graphql_string;
use crate::retry::execute_graphql_with_conflict_retry;

const EXACT_TRANSITION_ATTEMPTS: usize = 3;

enum ExactTransitionResult {
    Complete,
    RetryExpectedState,
}

enum ExactTransitionFacts {
    Running {
        started_at: String,
    },
    Terminal {
        ended_at: String,
        failure_reason: Option<String>,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
    },
}

impl ExactTransitionFacts {
    fn matches(&self, row: &serde_json::Value) -> bool {
        match self {
            Self::Running { started_at } => {
                row.get("started_at").and_then(serde_json::Value::as_str)
                    == Some(started_at.as_str())
            }
            Self::Terminal {
                ended_at,
                failure_reason,
                prompt_tokens,
                completion_tokens,
                cached_input_tokens,
            } => {
                row.get("ended_at").and_then(serde_json::Value::as_str) == Some(ended_at.as_str())
                    && row
                        .get("failure_reason")
                        .and_then(serde_json::Value::as_str)
                        == failure_reason.as_deref()
                    && row.get("prompt_tokens").and_then(serde_json::Value::as_u64)
                        == *prompt_tokens
                    && row
                        .get("completion_tokens")
                        .and_then(serde_json::Value::as_u64)
                        == *completion_tokens
                    && row
                        .get("cached_input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        == *cached_input_tokens
            }
        }
    }
}

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
    let resp = execute_graphql_with_conflict_retry(
        node.as_ref(),
        &mutation,
        "persist queued InferenceCall",
    )
    .await;
    if resp.has_errors() {
        anyhow::bail!("persisting queued InferenceCall failed: {:?}", resp.errors);
    }
    let doc_id = extract_inference_call_doc_id(resp.data.as_ref())?;
    if let Err(twin_error) =
        verify_no_logical_call_twin(node.as_ref(), &doc_id, &call.call_id).await
    {
        let cleanup = persist_existing_call_terminal(
            node,
            &doc_id,
            call,
            "queued",
            "cancelled",
            Some("Cancelled"),
            None,
        )
        .await;
        if let Err(cleanup_error) = cleanup {
            anyhow::bail!(
                "{twin_error:#}; quarantining newly created queued _docID={doc_id} also failed: {cleanup_error:#}"
            );
        }
        return Err(twin_error);
    }
    Ok(doc_id)
}

pub(super) async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, CompletionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp = execute_graphql_with_conflict_retry(
        node.as_ref(),
        &mutation,
        "persist running InferenceCall",
    )
    .await;
    if resp.has_errors() {
        return Err(CompletionError::ProviderError(format!(
            "persisting running InferenceCall failed: {:?}",
            resp.errors
        )));
    }
    let doc_id =
        extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)?;
    if let Err(twin_error) =
        verify_no_logical_call_twin(node.as_ref(), &doc_id, &call.call_id).await
    {
        let cleanup = persist_existing_call_terminal(
            node,
            &doc_id,
            call,
            "running",
            "failed",
            Some("StreamDroppedBeforeTerminalResponse"),
            None,
        )
        .await;
        let error = match cleanup {
            Ok(()) => twin_error,
            Err(cleanup_error) => anyhow::anyhow!(
                "{twin_error:#}; quarantining newly created running _docID={doc_id} also failed: {cleanup_error:#}"
            ),
        };
        return Err(completion_persistence_error(error));
    }
    Ok(doc_id)
}

pub(super) async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    doc_id: &str,
    call: &InferenceCallRecord,
) -> Result<()> {
    verify_no_logical_call_twin(node.as_ref(), doc_id, &call.call_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = update_call_running_mutation(doc_id, call, &now);
    let facts = ExactTransitionFacts::Running { started_at: now };
    execute_exact_transition_with_retry(
        node,
        &mutation,
        "persist existing running InferenceCall",
        doc_id,
        call,
        "queued",
        "running",
        &facts,
    )
    .await
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
    let resp = execute_graphql_with_conflict_retry(
        node.as_ref(),
        &mutation,
        "persist terminal InferenceCall",
    )
    .await;
    if resp.has_errors() {
        anyhow::bail!(
            "persisting terminal InferenceCall failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

pub(super) async fn persist_existing_call_terminal(
    node: Arc<EmbeddedNode>,
    doc_id: &str,
    call: &InferenceCallRecord,
    expected_call_state: &str,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let facts = ExactTransitionFacts::Terminal {
        ended_at: now.clone(),
        failure_reason: failure_reason.map(str::to_owned),
        prompt_tokens: usage.map(|usage| usage.input_tokens),
        completion_tokens: usage.map(|usage| usage.output_tokens),
        cached_input_tokens: usage.map(|usage| usage.cached_input_tokens),
    };
    let mutation = update_call_terminal_mutation(
        doc_id,
        call,
        expected_call_state,
        call_state,
        failure_reason,
        &now,
        usage,
    );
    execute_exact_transition_with_retry(
        node,
        &mutation,
        "persist existing terminal InferenceCall",
        doc_id,
        call,
        expected_call_state,
        call_state,
        &facts,
    )
    .await
}

async fn execute_exact_transition_with_retry(
    node: Arc<EmbeddedNode>,
    mutation: &str,
    operation: &str,
    doc_id: &str,
    call: &InferenceCallRecord,
    expected_call_state: &str,
    target_call_state: &str,
    facts: &ExactTransitionFacts,
) -> Result<()> {
    for attempt in 1..=EXACT_TRANSITION_ATTEMPTS {
        let response =
            execute_graphql_with_conflict_retry(node.as_ref(), mutation, operation).await;
        if response.has_errors() {
            anyhow::bail!("{operation} failed: {:?}", response.errors);
        }
        match verify_exact_transition(
            node.as_ref(),
            response.data.as_ref(),
            doc_id,
            call,
            expected_call_state,
            target_call_state,
            facts,
        )
        .await?
        {
            ExactTransitionResult::Complete => return Ok(()),
            ExactTransitionResult::RetryExpectedState if attempt < EXACT_TRANSITION_ATTEMPTS => {
                tokio::task::yield_now().await;
            }
            ExactTransitionResult::RetryExpectedState => {
                anyhow::bail!(
                    "InferenceCall exact transition remained in expected state after {EXACT_TRANSITION_ATTEMPTS} attempts: _docID={doc_id} call_id={} expected_state={expected_call_state} target_state={target_call_state}",
                    call.call_id
                );
            }
        }
    }
    unreachable!("bounded exact-transition loop always returns")
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

fn update_call_running_mutation(
    doc_id: &str,
    call: &InferenceCallRecord,
    started_at: &str,
) -> String {
    format!(
        r#"mutation {{
            update_InferenceCall(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    call_id: {{ _eq: "{call_id}" }},
                    runtime_instance_id: {{ _eq: "{runtime_instance_id}" }},
                    request_id: {{ _eq: "{request_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    controller_generation: {{ _eq: {controller_generation} }},
                    call_state: {{ _eq: "queued" }}
                }},
                input: {{
                    call_state: "running",
                    started_at: "{started_at}"
                }}
            ) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(doc_id),
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        agent_did = escape_graphql_string(&call.agent_did),
        controller_generation = call.controller_generation,
        started_at = escape_graphql_string(started_at),
    )
}

fn update_call_terminal_mutation(
    doc_id: &str,
    call: &InferenceCallRecord,
    expected_call_state: &str,
    call_state: &str,
    failure_reason: Option<&str>,
    ended_at: &str,
    usage: Option<Usage>,
) -> String {
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens, cached_input_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            update_InferenceCall(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    call_id: {{ _eq: "{call_id}" }},
                    runtime_instance_id: {{ _eq: "{runtime_instance_id}" }},
                    request_id: {{ _eq: "{request_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    controller_generation: {{ _eq: {controller_generation} }},
                    call_state: {{ _eq: "{expected_call_state}" }}
                }},
                input: {{
                    call_state: "{call_state}",
                    {failure_reason}
                    ended_at: "{ended_at}"
                    {prompt_tokens}
                    {completion_tokens}
                    {cached_input_tokens}
                }}
            ) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(doc_id),
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        agent_did = escape_graphql_string(&call.agent_did),
        controller_generation = call.controller_generation,
        expected_call_state = escape_graphql_string(expected_call_state),
        call_state = call_state,
        failure_reason = failure_reason,
        ended_at = escape_graphql_string(ended_at),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
        cached_input_tokens = cached_input_tokens,
    )
}

async fn verify_exact_transition(
    node: &EmbeddedNode,
    data: Option<&serde_json::Value>,
    doc_id: &str,
    call: &InferenceCallRecord,
    expected_call_state: &str,
    target_call_state: &str,
    facts: &ExactTransitionFacts,
) -> Result<ExactTransitionResult> {
    let returned_doc_ids = mutation_doc_ids(data, "update_InferenceCall");
    if returned_doc_ids.as_slice() == [doc_id] {
        return Ok(ExactTransitionResult::Complete);
    }
    if !returned_doc_ids.is_empty() {
        anyhow::bail!(
            "InferenceCall exact transition returned unexpected document ids: _docID={doc_id} call_id={} expected_state={expected_call_state} target_state={target_call_state} returned_doc_ids={returned_doc_ids:?}",
            call.call_id
        );
    }

    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            InferenceCall(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}) {{
                _docID
                call_id
                runtime_instance_id
                request_id
                agent_did
                controller_generation
                call_state
                started_at
                ended_at
                failure_reason
                prompt_tokens
                completion_tokens
                cached_input_tokens
            }}
        }}"#
    );
    let current = node.execute(&query).await;
    if current.has_errors() {
        anyhow::bail!(
            "InferenceCall transition returned document ids {returned_doc_ids:?}, then exact reload failed for _docID={doc_id}: {:?}",
            current.errors
        );
    }
    let observed = current
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first());
    let Some(observed) = observed else {
        anyhow::bail!(
            "InferenceCall exact transition matched no document and exact reload found no row: _docID={doc_id} call_id={} expected_state={expected_call_state} target_state={target_call_state}",
            call.call_id
        );
    };
    let observed_call_id = observed.get("call_id").and_then(serde_json::Value::as_str);
    let observed_call_state = observed
        .get("call_state")
        .and_then(serde_json::Value::as_str);
    let observed_identity_matches = observed_call_id == Some(call.call_id.as_str())
        && observed
            .get("runtime_instance_id")
            .and_then(serde_json::Value::as_str)
            == Some(call.runtime_instance_id.as_str())
        && observed
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            == Some(call.request_id.as_str())
        && observed
            .get("agent_did")
            .and_then(serde_json::Value::as_str)
            == Some(call.agent_did.as_str())
        && observed
            .get("controller_generation")
            .and_then(serde_json::Value::as_u64)
            == Some(call.controller_generation);
    if observed_identity_matches {
        if observed_call_state == Some(target_call_state) {
            if facts.matches(observed) {
                return Ok(ExactTransitionResult::Complete);
            }
            anyhow::bail!(
                "InferenceCall exact transition reached target state with conflicting facts: _docID={doc_id} call_id={} target_state={target_call_state}",
                call.call_id
            );
        }
        if observed_call_state == Some(expected_call_state) {
            return Ok(ExactTransitionResult::RetryExpectedState);
        }
    }
    anyhow::bail!(
        "InferenceCall exact transition conflict: _docID={doc_id} call_id={} expected_state={expected_call_state} target_state={target_call_state} returned_doc_ids={returned_doc_ids:?} observed_call_id={observed_call_id:?} observed_state={observed_call_state:?}",
        call.call_id
    )
}

async fn verify_no_logical_call_twin(
    node: &EmbeddedNode,
    doc_id: &str,
    call_id: &str,
) -> Result<()> {
    let escaped_call_id = escape_graphql_string(call_id);
    let query = format!(
        r#"{{
            InferenceCall(filter: {{ call_id: {{ _eq: "{escaped_call_id}" }} }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "checking InferenceCall logical uniqueness for call_id={call_id}: {:?}",
            response.errors
        );
    }
    let doc_ids = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("_docID").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    if doc_ids.as_slice() == [doc_id] {
        return Ok(());
    }
    anyhow::bail!(
        "InferenceCall logical identity conflict for call_id={call_id}: created _docID={doc_id}, visible _docIDs={doc_ids:?}"
    )
}

fn mutation_doc_ids(data: Option<&serde_json::Value>, field: &str) -> Vec<String> {
    let Some(value) = data.and_then(|data| data.get(field)) else {
        return Vec::new();
    };
    if let Some(doc_id) = value.get("_docID").and_then(serde_json::Value::as_str) {
        return vec![doc_id.to_owned()];
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("_docID")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
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
    use crate::admission::CallKind;
    use crate::schema::ensure_schemas;

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ensure_schemas(node.as_ref()).await.unwrap();
        node
    }

    fn call(call_id: &str, request_id: &str) -> InferenceCallRecord {
        InferenceCallRecord {
            call_id: call_id.to_owned(),
            runtime_instance_id: "runtime-exact-target-test".to_owned(),
            request_id: request_id.to_owned(),
            call_seq: 1,
            backend_id: "backend-exact-target-test".to_owned(),
            behavior_id: "default".to_owned(),
            agent_did: "did:key:exact-target-test".to_owned(),
            call_kind: CallKind::Inference,
            attempt: 1,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "exact-target-test".to_owned(),
        }
    }

    async fn call_identity_and_state(node: &EmbeddedNode, doc_id: &str) -> (String, String) {
        let doc_id = escape_graphql_string(doc_id);
        let response = node
            .execute(&format!(
                r#"{{
                    InferenceCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{
                        call_id
                        call_state
                    }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let row = response
            .data
            .as_ref()
            .and_then(|data| data.get("InferenceCall"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .expect("exact InferenceCall row");
        (
            row["call_id"].as_str().unwrap().to_owned(),
            row["call_state"].as_str().unwrap().to_owned(),
        )
    }

    #[tokio::test]
    async fn existing_transitions_reject_sibling_and_stale_document_mutation() {
        let node = test_node().await;
        let first = call("call-exact-first", "request-exact-first");
        let sibling = call("call-exact-sibling", "request-exact-sibling");
        let first_doc_id = persist_call_queued(node.clone(), &first).await.unwrap();
        let sibling_doc_id = persist_call_queued(node.clone(), &sibling).await.unwrap();

        persist_existing_call_running(node.clone(), &first_doc_id, &first)
            .await
            .unwrap();
        let sibling_error = persist_existing_call_running(node.clone(), &sibling_doc_id, &first)
            .await
            .unwrap_err();
        assert!(
            sibling_error
                .to_string()
                .contains("InferenceCall logical identity conflict"),
            "{sibling_error:#}"
        );
        assert_eq!(
            call_identity_and_state(node.as_ref(), &sibling_doc_id).await,
            (sibling.call_id.clone(), "queued".to_owned())
        );

        persist_existing_call_terminal(
            node.clone(),
            &first_doc_id,
            &first,
            "running",
            "completed",
            None,
            None,
        )
        .await
        .unwrap();
        let stale_error = persist_existing_call_terminal(
            node.clone(),
            &first_doc_id,
            &first,
            "running",
            "failed",
            Some("StreamDroppedBeforeTerminalResponse"),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            stale_error
                .to_string()
                .contains("observed_state=Some(\"completed\")"),
            "{stale_error:#}"
        );
        assert_eq!(
            call_identity_and_state(node.as_ref(), &first_doc_id).await,
            (first.call_id, "completed".to_owned())
        );
    }
}
