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
            request_id: "recovery-cas-request", call_seq: 1,
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
