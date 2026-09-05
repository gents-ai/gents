use super::*;
use serde_json::Value;

const AGENT_DID: &str = "did:test:inference-recovery-cas";

async fn fixture(initial_state: &str) -> (EmbeddedNode, StaleInferenceCallRow) {
    let node = EmbeddedNode::builder().build().await.unwrap();
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let state = escape_graphql_string(initial_state);
    let response = node
        .execute(&format!(
            r#"mutation {{ add_InferenceCall(input: {{
            call_id: "recovery-cas-call", runtime_instance_id: "recovery-cas-runtime",
            request_id: "recovery-cas-request", request_doc_id: "recovery-cas-request-doc", call_seq: 1,
            backend_id: "recovery-cas-backend", behavior_id: "test", agent_did: "{AGENT_DID}",
            call_kind: "inference", attempt: 1, call_state: "{state}",
            queued_at: "2026-09-01T00:00:00Z", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let mut snapshots = load_stale_inference_calls(&node, AGENT_DID).await.unwrap();
    assert_eq!(snapshots.len(), 1);
    (node, snapshots.remove(0))
}

async fn stored_call(node: &EmbeddedNode) -> Value {
    let response = node.execute(
        r#"{ InferenceCall { call_state failure_reason ended_at prompt_tokens completion_tokens } }"#,
    ).await;
    crate::graphql::rows::<Value>(&response, "InferenceCall")
        .unwrap()
        .remove(0)
}

/// Lean InferenceCall's terminal states have no outgoing transitions. A sweep
/// that observed running before the owner completed must preserve that outcome.
#[tokio::test]
async fn stale_recovery_preserves_concurrent_completion_and_usage() {
    let (node, snapshot) = fixture("running").await;
    let doc_id = escape_graphql_string(&snapshot.doc_id);
    let response = node
        .execute(&format!(
            r#"mutation {{ update_InferenceCall(
            filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
            input: {{ call_state: "completed", ended_at: "2026-09-02T00:00:00Z",
                prompt_tokens: 17, completion_tokens: 9 }}
        ) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let completed = stored_call(&node).await;
    assert!(
        !recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Failed)
            .await
            .unwrap()
    );
    assert_eq!(stored_call(&node).await, completed);
}

#[tokio::test]
async fn competing_recovery_counts_only_the_winner_and_preserves_terminal_stamp() {
    let (node, snapshot) = fixture("running").await;
    let winner = recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Failed)
        .await
        .unwrap();
    assert!(winner);
    let terminal = stored_call(&node).await;
    assert_eq!(terminal["call_state"], "failed");
    let loser = recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Failed)
        .await
        .unwrap();
    assert!(!loser);
    assert_eq!(usize::from(winner) + usize::from(loser), 1);
    assert_eq!(stored_call(&node).await, terminal);
}

#[tokio::test]
async fn stale_queued_recovery_cannot_cancel_a_running_call() {
    let (node, snapshot) = fixture("queued").await;
    let doc_id = escape_graphql_string(&snapshot.doc_id);
    let response = node
        .execute(&format!(
            r#"mutation {{ update_InferenceCall(
            filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
            input: {{ call_state: "running" }}
        ) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let running = stored_call(&node).await;
    assert!(
        !recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Cancelled)
            .await
            .unwrap()
    );
    assert_eq!(stored_call(&node).await, running);
}

fn pending_call(call_id: &str) -> super::super::controller::PendingCallMetadata {
    super::super::controller::PendingCallMetadata {
        call_id: call_id.into(),
        runtime_instance_id: "recovery-cas-runtime".into(),
        request_id: "recovery-cas-request".into(),
        request_doc_id: "recovery-cas-request-doc".into(),
        call_seq: 1,
        backend_id: "recovery-cas-backend".into(),
        behavior_id: "test".into(),
        agent_did: AGENT_DID.into(),
        call_kind: super::super::CallKind::Inference,
        attempt: 1,
    }
}

/// Lean: InferenceCall.Persistence.recovery_winner_cannot_be_reopened.
#[tokio::test]
async fn recovery_winner_rejects_late_running_persistence() {
    let (node, snapshot) = fixture("queued").await;
    let node = std::sync::Arc::new(node);
    assert!(
        recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Cancelled)
            .await
            .unwrap()
    );
    let terminal = stored_call(&node).await;
    let call = super::super::controller::InferenceCallRecord::without_controller(pending_call(
        "recovery-cas-call",
    ));
    assert!(
        super::super::persistence::persist_existing_call_running(node.clone(), &call)
            .await
            .is_err()
    );
    assert_eq!(stored_call(&node).await, terminal);
}

/// Exercise the real queued acquisition path: losing the durable start must
/// return its semaphore/bookkeeping and never hand provider code a permit.
#[tokio::test]
async fn recovered_queued_call_cannot_acquire_provider_permit() {
    use super::super::controller::BackendAdmissionController;
    let node = std::sync::Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let controller = BackendAdmissionController::new(
        1,
        super::super::BackendAdmissionConfig {
            backend_id: "recovery-cas-backend".into(),
            max_concurrent: 1,
            max_queue_depth: 1,
            enabled: true,
            probe_status: "healthy".into(),
            measured_unhealthy: false,
            config_fingerprint: "recovery-cas".into(),
        },
        std::sync::Weak::new(),
    );
    let mut holder = controller
        .clone()
        .acquire(node.clone(), pending_call("holder-call"), None, None)
        .await
        .unwrap();
    let waiting_controller = controller.clone();
    let waiting_node = node.clone();
    let waiting = tokio::spawn(async move {
        waiting_controller
            .acquire(waiting_node, pending_call("waiting-call"), None, None)
            .await
    });
    let queued = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(row) = load_stale_inference_calls(&node, AGENT_DID)
                .await
                .unwrap()
                .into_iter()
                .find(|row| row.call_id == "waiting-call" && row.call_state == "queued")
            {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiting call must persist its queue row");
    assert!(
        recover_inference_call_row(&node, &queued, InferenceRecoveryOutcome::Cancelled)
            .await
            .unwrap()
    );
    holder.finish_success(None).await.unwrap();
    drop(holder);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .expect("recovered waiter must finish")
        .unwrap();
    assert!(
        result.is_err(),
        "a recovered queued call must not obtain a provider permit"
    );
    assert!(
        controller.is_drained(),
        "losing admission must release controller bookkeeping"
    );
    let response = node
        .execute(r#"{ InferenceCall(filter: {call_id: {_eq: "waiting-call"}}) {call_state} }"#)
        .await;
    let rows = crate::graphql::rows::<Value>(&response, "InferenceCall").unwrap();
    assert_eq!(rows[0]["call_state"], "cancelled");
}

/// Lean namespace InferenceCall.Persistence: terminal_winner_preserves_outcome_and_stamp and
/// late_usage_preserves_terminal_projection / late_usage_is_recorded.
#[tokio::test]
async fn recovery_winner_preserves_terminal_stamp_and_rehydrates_late_usage() {
    let (node, snapshot) = fixture("running").await;
    let node = std::sync::Arc::new(node);
    assert!(
        recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Failed)
            .await
            .unwrap()
    );
    let terminal = stored_call(&node).await;
    let call = super::super::controller::InferenceCallRecord::without_controller(pending_call(
        "recovery-cas-call",
    ));
    let usage = rig::completion::Usage {
        input_tokens: 17,
        output_tokens: 9,
        total_tokens: 26,
        cached_input_tokens: 3,
        cache_creation_input_tokens: 0,
    };
    super::super::persistence::persist_existing_call_terminal(
        node.clone(),
        &call,
        "completed",
        None,
        Some(usage),
    )
    .await
    .unwrap();
    let stored = stored_call(&node).await;
    for field in ["call_state", "failure_reason", "ended_at"] {
        assert_eq!(
            stored[field], terminal[field],
            "late completion changed terminal {field}"
        );
    }
    assert_eq!(stored["prompt_tokens"], 17);
    assert_eq!(stored["completion_tokens"], 9);
    let mut request = super::super::tests::request("recovery-cas-request");
    request.doc_id = "recovery-cas-request-doc".into();
    request.max_total_tokens = Some(100);
    let budget = crate::completion_factory::aggregate_token_budget_for_request(&node, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        budget.snapshot().unwrap().remaining(),
        74,
        "recovery must not discard observed usage on restart"
    );
    // A replayed late write may not create an additional row or charge twice.
    super::super::persistence::persist_existing_call_terminal(
        node.clone(),
        &call,
        "completed",
        None,
        Some(usage),
    )
    .await
    .unwrap();
    assert_eq!(stored_call(&node).await, stored);
    let budget = crate::completion_factory::aggregate_token_budget_for_request(&node, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(budget.snapshot().unwrap().remaining(), 74);
}
