use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    scope_call, scope_call_with_token, scope_request, AdmissionCallContext, AdmissionRegistry,
    BackendAdmissionConfig, CallKind,
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
        created_at: "2026-04-15T00:00:00Z".to_string(),
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
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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
        first.finish_success(None).await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "failed");
    assert_eq!(rows[1]["failure_reason"], "QueueFull");
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
}

#[tokio::test]
async fn dropped_permit_with_cancelled_token_persists_cancelled_terminal() {
    // Validates the Composed.lean::interrupted_request_cancels_calls_PLACEHOLDER
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
}
