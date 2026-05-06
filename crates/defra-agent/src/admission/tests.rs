use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    scope_call, scope_call_with_token, scope_request, AdmissionCallContext, AdmissionRegistry,
    BackendAdmissionConfig, CallKind,
};
use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_set_matches, assert_lean_transition_is_illegal,
    assert_lean_transition_is_legal, assert_state_machine_contract_is_complete,
    lean_vocabulary_values, LeanContractVocabulary,
};
use crate::schema::ensure_schemas;
use crate::watcher::AgentRequest;

async fn test_node() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_schemas(node.as_ref()).await.unwrap();
    node
}

fn config(
    backend_id: &str,
    max_concurrent: usize,
    max_queue_depth: usize,
) -> BackendAdmissionConfig {
    BackendAdmissionConfig {
        backend_id: backend_id.to_string(),
        max_concurrent,
        max_queue_depth,
        enabled: true,
        probe_status: "healthy".to_string(),
        config_fingerprint: format!("{backend_id}:{max_concurrent}:{max_queue_depth}"),
    }
}

fn request(request_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: format!("doc-{request_id}"),
        request_id: request_id.to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("default".to_string()),
        session_id: format!("session-{request_id}"),
        content: "hello".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: "2026-04-15T00:00:00Z".to_string(),
    }
}

const ADMISSION_TERMINAL_REASON_SOURCES: &[&str] = &[
    include_str!("controller.rs"),
    include_str!("permit.rs"),
    include_str!("registry.rs"),
];
const ADMISSION_CALL_STATE_SOURCES: &[&str] = &[
    include_str!("controller.rs"),
    include_str!("permit.rs"),
    include_str!("persistence.rs"),
    include_str!("registry.rs"),
];

fn lean_inference_call_states() -> Vec<&'static str> {
    lean_vocabulary_values("InferenceCallState")
}

fn string_literals_after(source: &'static str, needle: &str) -> Vec<&'static str> {
    let mut rest = source;
    let mut values = Vec::new();
    while let Some(start) = rest.find(needle) {
        let value_start = start + needle.len();
        let after_start = &rest[value_start..];
        let value_end = after_start.find('"').expect("string literal must close");
        values.push(&after_start[..value_end]);
        rest = &after_start[value_end + 1..];
    }
    values
}

fn rust_literal_terminal_reasons_from_admission_sources() -> Vec<&'static str> {
    let mut values = ADMISSION_TERMINAL_REASON_SOURCES
        .iter()
        .flat_map(|source| string_literals_after(source, "Some(\""))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

fn rust_literal_call_states_from_admission_sources() -> Vec<&'static str> {
    let patterns = [
        "call_state: \"",
        "add_call_mutation(call, \"",
        "persist_terminal_call(node, call, \"",
        "persist_existing_call_terminal(node, &call, \"",
    ];
    let mut values = ADMISSION_CALL_STATE_SOURCES
        .iter()
        .flat_map(|source| {
            patterns
                .iter()
                .flat_map(|pattern| string_literals_after(source, pattern))
        })
        .filter(|value| !value.contains('{'))
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

fn assert_inference_call_rows_use_lean_vocabulary(rows: &[Value]) {
    let lean_states = lean_inference_call_states();
    for row in rows {
        let state = row
            .get("call_state")
            .and_then(Value::as_str)
            .expect("InferenceCall row must include call_state");
        assert!(
            lean_states.contains(&state),
            "InferenceCall.call_state={state:?} is not in the Lean InferenceCallState vocabulary"
        );
    }
}

async fn call_rows(node: &EmbeddedNode) -> Vec<Value> {
    let response = node
        .execute(
            r#"{
                InferenceCall(order: { call_seq: ASC }) {
                    request_id
                    call_seq
                    backend_id
                    behavior_id
                    call_kind
                    call_state
                    failure_reason
                    queue_depth_at_enqueue
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_inference_call_rows_use_lean_vocabulary(&rows);
    rows
}

fn running_slot_count_for_backend(rows: &[Value], backend_id: &str) -> usize {
    rows.iter()
        .filter(|row| {
            row.get("backend_id").and_then(Value::as_str) == Some(backend_id)
                && row.get("call_state").and_then(Value::as_str) == Some("running")
        })
        .count()
}

fn state_count_for_backend(rows: &[Value], backend_id: &str, call_state: &str) -> usize {
    rows.iter()
        .filter(|row| {
            row.get("backend_id").and_then(Value::as_str) == Some(backend_id)
                && row.get("call_state").and_then(Value::as_str) == Some(call_state)
        })
        .count()
}

fn assert_reconstructed_slot_count(rows: &[Value], backend_id: &str, expected: usize) {
    assert_eq!(
        running_slot_count_for_backend(rows, backend_id),
        expected,
        "held backend slots are reconstructed from persisted InferenceCall rows with call_state=running"
    );
}

fn assert_reconstructed_slot_count_at_most(
    rows: &[Value],
    backend_id: &str,
    max_concurrent: usize,
) {
    let reconstructed = running_slot_count_for_backend(rows, backend_id);
    assert!(
        reconstructed <= max_concurrent,
        "reconstructed running-row slot count {reconstructed} exceeded max_concurrent {max_concurrent}"
    );
}

async fn wait_for_call_row_count(node: &EmbeddedNode, expected: usize) -> Vec<Value> {
    let mut last = Vec::new();
    for _ in 0..100 {
        let rows = call_rows(node).await;
        if rows.len() >= expected {
            return rows;
        }
        last = rows;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for at least {expected} InferenceCall rows, last rows={last:?}");
}

async fn wait_for_request_call_state(
    node: &EmbeddedNode,
    request_id: &str,
    expected_state: &str,
) -> Value {
    let mut last = Vec::new();
    for _ in 0..100 {
        let rows = call_rows(node).await;
        if let Some(row) = rows.iter().find(|row| {
            row.get("request_id").and_then(Value::as_str) == Some(request_id)
                && row.get("call_state").and_then(Value::as_str) == Some(expected_state)
        }) {
            return row.clone();
        }
        last = rows;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for request_id={request_id} InferenceCall state={expected_state}, last rows={last:?}"
    );
}

#[test]
fn rust_inference_call_state_vocabulary_matches_lean_model() {
    let rust_states = rust_literal_call_states_from_admission_sources();
    assert_lean_contract_vocabulary_set_matches(LeanContractVocabulary {
        domain: "InferenceCallState",
        rust_source: "admission source call_state literals",
        rust_values: &rust_states,
    });
}

#[test]
fn rust_inference_call_terminal_reason_vocabulary_matches_lean_model() {
    let rust_reasons = rust_literal_terminal_reasons_from_admission_sources();
    assert_lean_contract_vocabulary_set_matches(LeanContractVocabulary {
        domain: "InferenceCallTerminalReason",
        rust_source: "admission system terminal reason literals",
        rust_values: &rust_reasons,
    });
}

#[test]
fn rust_inference_call_transition_table_matches_lean_contract() {
    assert_state_machine_contract_is_complete("InferenceCall");
    assert_lean_transition_is_legal("InferenceCall", "queued", "running");
    assert_lean_transition_is_legal("InferenceCall", "queued", "cancelled");
    assert_lean_transition_is_legal("InferenceCall", "running", "completed");
    assert_lean_transition_is_legal("InferenceCall", "running", "failed");
    assert_lean_transition_is_legal("InferenceCall", "running", "cancelled");
    assert_lean_transition_is_illegal("InferenceCall", "queued", "completed");
    assert_lean_transition_is_illegal("InferenceCall", "completed", "running");
}

#[tokio::test]
async fn missing_backend_persists_backend_gone_cancelled_terminal() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    let context =
        AdmissionCallContext::for_request(&request("req-backend-gone"), "default", "missing");

    scope_request(context, async {
        let error = match registry.acquire_current_call().await {
            Ok(_) => panic!("missing backend should reject without a permit"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("BackendGone"));
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["backend_id"], "missing");
    assert_eq!(rows[0]["call_state"], "cancelled");
    assert_eq!(rows[0]["failure_reason"], "BackendGone");
    assert_reconstructed_slot_count(&rows, "missing", 0);
}

#[tokio::test]
async fn max_queue_depth_zero_allows_immediate_permit_and_rejects_saturated_backend() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 0))]),
    );
    let context = AdmissionCallContext::for_request(&request("req-zero"), "default", "backend-a");

    scope_request(context, async {
        let mut first = registry.acquire_current_call().await.unwrap();
        let error = match registry.acquire_current_call().await {
            Ok(_) => panic!("saturated backend should reject without queue capacity"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("QueueFull"));
        let rows = wait_for_call_row_count(node.as_ref(), 2).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "running");
        assert_eq!(rows[1]["call_state"], "failed");
        assert_eq!(rows[1]["failure_reason"], "QueueFull");
        assert_reconstructed_slot_count(&rows, "backend-a", 1);
        first.finish_success(None).await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "failed");
    assert_eq!(rows[1]["failure_reason"], "QueueFull");
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn reconstructed_running_rows_never_exceed_max_concurrent_under_contention() {
    const TASKS: usize = 5;
    const MAX_CONCURRENT: usize = 2;

    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([(
            "backend-a".to_string(),
            config("backend-a", MAX_CONCURRENT, TASKS),
        )]),
    );

    let (acquired_tx, mut acquired_rx) = mpsc::unbounded_channel::<usize>();
    let mut release_senders = HashMap::new();
    let mut handles = Vec::new();

    for idx in 0..TASKS {
        let context = AdmissionCallContext::for_request(
            &request(&format!("req-contention-{idx}")),
            "default",
            "backend-a",
        );
        let task_registry = registry.clone();
        let task_acquired_tx = acquired_tx.clone();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        release_senders.insert(idx, release_tx);

        handles.push(tokio::spawn(async move {
            scope_request(context, async move {
                let mut permit = task_registry.acquire_current_call().await.unwrap();
                task_acquired_tx
                    .send(idx)
                    .expect("test acquired receiver must stay open");
                let _ = release_rx.await;
                permit.finish_success(None).await;
            })
            .await;
        }));
    }
    drop(acquired_tx);

    let mut acquired = Vec::new();
    while acquired.len() < MAX_CONCURRENT {
        acquired.push(
            acquired_rx
                .recv()
                .await
                .expect("expected initial permits to acquire"),
        );
    }

    let rows = wait_for_call_row_count(node.as_ref(), TASKS).await;
    assert_reconstructed_slot_count(&rows, "backend-a", MAX_CONCURRENT);
    assert_reconstructed_slot_count_at_most(&rows, "backend-a", MAX_CONCURRENT);
    assert_eq!(state_count_for_backend(&rows, "backend-a", "queued"), 3);

    let released = acquired[0];
    release_senders
        .remove(&released)
        .expect("release sender for acquired permit")
        .send(())
        .expect("held task should still be waiting for release");
    acquired.push(
        acquired_rx
            .recv()
            .await
            .expect("queued task should acquire after one permit release"),
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_reconstructed_slot_count(&rows, "backend-a", MAX_CONCURRENT);
    assert_reconstructed_slot_count_at_most(&rows, "backend-a", MAX_CONCURRENT);
    assert_eq!(state_count_for_backend(&rows, "backend-a", "completed"), 1);

    for (_, release_tx) in release_senders {
        let _ = release_tx.send(());
    }
    for handle in handles {
        handle.await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
    assert_eq!(
        state_count_for_backend(&rows, "backend-a", "completed"),
        TASKS
    );
}

#[tokio::test]
async fn queued_calls_start_in_tokio_registration_order_after_permit_release() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 2))]),
    );
    let first_context =
        AdmissionCallContext::for_request(&request("req-ordered"), "default", "backend-a");
    let second_context = first_context.clone();

    scope_request(first_context, async {
        let mut first = registry.acquire_current_call().await.unwrap();
        let second_registry = registry.clone();
        let second = tokio::spawn(async move {
            scope_request(second_context, async move {
                let mut permit = second_registry.acquire_current_call().await.unwrap();
                permit.finish_success(None).await;
            })
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "running");
        assert_eq!(rows[1]["call_state"], "queued");
        assert_eq!(
            running_slot_count_for_backend(&rows, "backend-a"),
            1,
            "the aggregate slot count is reconstructed from running InferenceCall rows; queued rows do not hold slots"
        );

        first.finish_success(None).await;
        drop(first);
        second.await.unwrap();
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "completed");
    assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
    assert_eq!(
        running_slot_count_for_backend(&rows, "backend-a"),
        0,
        "terminal InferenceCall rows reconstruct zero held scheduler slots"
    );
}

#[tokio::test]
async fn cancelling_queued_call_terminalizes_without_holding_slot() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let running_context =
        AdmissionCallContext::for_request(&request("req-running-holder"), "default", "backend-a");
    let queued_context =
        AdmissionCallContext::for_request(&request("req-queued-cancel"), "default", "backend-a");

    scope_request(running_context, async {
        let mut first = registry.acquire_current_call().await.unwrap();
        let queued_registry = registry.clone();
        let queued = tokio::spawn(async move {
            scope_request(queued_context, async move {
                let _permit = queued_registry.acquire_current_call().await.unwrap();
            })
            .await;
        });

        wait_for_request_call_state(node.as_ref(), "req-queued-cancel", "queued").await;
        let rows = call_rows(node.as_ref()).await;
        assert_reconstructed_slot_count(&rows, "backend-a", 1);
        assert_eq!(state_count_for_backend(&rows, "backend-a", "queued"), 1);

        queued.abort();
        let _ = queued.await;
        wait_for_request_call_state(node.as_ref(), "req-queued-cancel", "cancelled").await;
        let rows = call_rows(node.as_ref()).await;
        assert_reconstructed_slot_count(&rows, "backend-a", 1);
        assert_eq!(state_count_for_backend(&rows, "backend-a", "cancelled"), 1);

        first.finish_success(None).await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
    assert_eq!(state_count_for_backend(&rows, "backend-a", "completed"), 1);
    assert_eq!(state_count_for_backend(&rows, "backend-a", "cancelled"), 1);
}

#[tokio::test]
async fn explicit_failure_releases_reconstructed_slot() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-explicit-failure"), "default", "backend-a");

    scope_request(context, async {
        let mut permit = registry.acquire_current_call().await.unwrap();
        let rows = call_rows(node.as_ref()).await;
        assert_reconstructed_slot_count(&rows, "backend-a", 1);
        permit.finish_failure("provider failed").await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_state"], "failed");
    assert_eq!(rows[0]["failure_reason"], "provider failed");
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn scoped_scheduled_calls_are_persisted_with_scheduled_kind() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-scheduled"), "default", "backend-a");

    scope_request(context, async {
        scope_call(CallKind::Scheduled, 1, async {
            let mut permit = registry.acquire_current_call().await.unwrap();
            permit.finish_success(None).await;
        })
        .await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_kind"], "scheduled");
    assert_eq!(rows[0]["call_state"], "completed");
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn compaction_calls_share_backend_capacity_with_inference_calls() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let inference_context =
        AdmissionCallContext::for_request(&request("req-compaction"), "default", "backend-a");
    let compaction_context = inference_context.clone();

    scope_request(inference_context, async {
        let mut inference = registry.acquire_current_call().await.unwrap();
        let compaction_registry = registry.clone();
        let compaction = tokio::spawn(async move {
            scope_request(compaction_context, async move {
                scope_call(CallKind::Compaction, 1, async {
                    let mut permit = compaction_registry.acquire_current_call().await.unwrap();
                    permit.finish_success(None).await;
                })
                .await;
            })
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_kind"], "inference");
        assert_eq!(rows[0]["call_state"], "running");
        assert_eq!(rows[1]["call_kind"], "compaction");
        assert_eq!(rows[1]["call_state"], "queued");
        assert_reconstructed_slot_count(&rows, "backend-a", 1);

        inference.finish_success(None).await;
        drop(inference);
        compaction.await.unwrap();
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "completed");
    assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn scoped_oneoff_calls_are_persisted_with_oneoff_kind() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context = AdmissionCallContext::for_request(&request("req-oneoff"), "default", "backend-a");

    scope_request(context, async {
        scope_call(CallKind::OneOff, 1, async {
            let mut permit = registry.acquire_current_call().await.unwrap();
            permit.finish_success(None).await;
        })
        .await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_kind"], "oneoff");
    assert_eq!(rows[0]["call_state"], "completed");
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn dropped_permit_with_cancelled_token_persists_cancelled_terminal() {
    // Validates the ComposedState::interrupted_request_cancels_live_linked_call
    // runtime bridge for the mid-stream path: if the inference_token is cancelled
    // at permit Drop time (e.g. daemon dropped the stream future because
    // the request was interrupted), the persisted InferenceCall row lands
    // as cancelled/Cancelled rather than the default
    // failed/StreamDroppedBeforeTerminalResponse fallback.
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-cancel-drop"), "default", "backend-a");

    let token = CancellationToken::new();
    token.cancel();

    scope_request(context, async {
        scope_call_with_token(CallKind::Inference, 1, token, async {
            let permit = registry.acquire_current_call().await.unwrap();
            let rows = call_rows(node.as_ref()).await;
            assert_reconstructed_slot_count(&rows, "backend-a", 1);
            // Drop without calling finish_success/finish_failure — simulates
            // the daemon dropping the stream future mid-stream after the
            // request-level cancellation token fires.
            drop(permit);
        })
        .await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_state"], "cancelled");
    assert_eq!(rows[0]["failure_reason"], "Cancelled");
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}

#[tokio::test]
async fn dropped_permit_without_cancelled_token_persists_failed_terminal() {
    // Protects the existing default-terminal behavior for non-interrupt
    // scenarios: when the inference_token is absent (or present but not
    // cancelled), a permit dropped without an explicit terminal still
    // lands as failed/StreamDroppedBeforeTerminalResponse — i.e. a real
    // provider-side stream drop, not a user interrupt.
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-default-drop"), "default", "backend-a");

    scope_request(context, async {
        let permit = registry.acquire_current_call().await.unwrap();
        let rows = call_rows(node.as_ref()).await;
        assert_reconstructed_slot_count(&rows, "backend-a", 1);
        drop(permit);
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_state"], "failed");
    assert_eq!(
        rows[0]["failure_reason"],
        "StreamDroppedBeforeTerminalResponse"
    );
    assert_reconstructed_slot_count(&rows, "backend-a", 0);
}
