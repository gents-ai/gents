use super::*;
use futures::{future::BoxFuture, StreamExt};
use serde_json::Value;

const NODE_DID: &str = "did:test:inference-recovery-cas";

async fn fixture(initial_state: &str) -> (std::sync::Arc<EmbeddedNode>, StaleInferenceCallRow) {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let snapshot = seed_call(&node, initial_state, NODE_DID, "recovery-cas-request-doc").await;
    (node, snapshot)
}

async fn seed_call(
    node: &std::sync::Arc<EmbeddedNode>,
    initial_state: &str,
    node_did: &str,
    request_doc_id: &str,
) -> StaleInferenceCallRow {
    let state = escape_graphql_string(initial_state);
    let owner = escape_graphql_string(node_did);
    let request_doc = escape_graphql_string(request_doc_id);
    crate::store::config_client::ConfigAccess::local(node.clone())
        .write(
            "test.recovery_call",
            &format!(
                r#"mutation {{ add_InferenceCall(input: {{
            call_id: "recovery-cas-call", runtime_instance_id: "recovery-cas-runtime",
            request_id: "recovery-cas-request", request_doc_id: "{request_doc}", call_seq: 1,
            backend_id: "recovery-cas-backend", agent_id: "test", node_did: "{owner}",
            call_kind: "inference", attempt: 1, call_state: "{state}",
            queued_at: "2026-09-01T00:00:00Z", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    let mut snapshots = load_stale_inference_calls(node, node_did).await.unwrap();
    assert_eq!(snapshots.len(), 1);
    snapshots.remove(0)
}

async fn stored_call(node: &EmbeddedNode) -> Value {
    let response = node.execute(
        r#"{ InferenceCall { call_state failure_reason ended_at prompt_tokens completion_tokens } }"#,
    ).await;
    crate::store::graphql::rows::<Value>(&response, "InferenceCall")
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
        session_id: "session-test".to_string(),
        model_name: None,
        profile_id: None,
        call_id: call_id.into(),
        runtime_instance_id: "recovery-cas-runtime".into(),
        request_id: "recovery-cas-request".into(),
        request_doc_id: "recovery-cas-request-doc".into(),
        call_seq: 1,
        backend_id: "recovery-cas-backend".into(),
        agent_id: "test".into(),
        node_did: NODE_DID.into(),
        call_kind: super::super::CallKind::Inference,
        attempt: 1,
    }
}

/// Lean: InferenceCall.Persistence.recovery_winner_cannot_be_reopened.
#[tokio::test]
async fn recovery_winner_rejects_late_running_persistence() {
    let (node, snapshot) = fixture("queued").await;
    assert!(
        recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Cancelled)
            .await
            .unwrap()
    );
    let terminal = stored_call(&node).await;
    let call = super::super::controller::InferenceCallRecord::without_controller(pending_call(
        "recovery-cas-call",
    ));
    assert!(super::super::persistence::persist_existing_call_running(
        node.clone(),
        &call,
        &snapshot.doc_id,
    )
    .await
    .is_err());
    assert_eq!(stored_call(&node).await, terminal);
}

#[tokio::test]
async fn physical_call_updates_preserve_identity_and_state_predicates() {
    use super::super::controller::InferenceCallRecord;
    use super::super::persistence::{
        persist_call_queued, persist_existing_call_running, persist_existing_call_terminal,
    };
    use gents_loop::rig_compat::CachedInputTokensObservation;

    let (node, snapshot) = fixture("queued").await;
    let call = InferenceCallRecord::without_controller(pending_call("recovery-cas-call"));
    let other = InferenceCallRecord::without_controller(pending_call("other-call"));
    let other_doc = persist_call_queued(node.clone(), &other).await.unwrap();
    assert!(
        persist_existing_call_running(node.clone(), &call, &other_doc)
            .await
            .is_err()
    );
    assert!(persist_existing_call_terminal(
        node.clone(),
        &call,
        &other_doc,
        "cancelled",
        None,
        None,
        CachedInputTokensObservation::NotAvailable,
    )
    .await
    .is_err());

    persist_existing_call_running(node.clone(), &call, &snapshot.doc_id)
        .await
        .unwrap();
    persist_existing_call_terminal(
        node.clone(),
        &call,
        &snapshot.doc_id,
        "completed",
        None,
        None,
        CachedInputTokensObservation::NotAvailable,
    )
    .await
    .unwrap();
    assert!(
        persist_existing_call_running(node.clone(), &call, &snapshot.doc_id)
            .await
            .is_err()
    );
    let response = crate::store::config_client::ConfigAccess::local(node)
        .execute("{ InferenceCall { call_id call_state } }")
        .await
        .unwrap();
    let rows = response["data"]["InferenceCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row["call_state"],
            if row["call_id"] == "recovery-cas-call" {
                "completed"
            } else {
                "queued"
            },
        );
    }
}

/// Exercise the real queued acquisition path: losing the durable start must
/// return its semaphore/bookkeeping and never hand provider code a permit.
#[tokio::test]
async fn recovered_queued_call_cannot_acquire_provider_permit() {
    use super::super::controller::BackendAdmissionController;
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
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
        super::super::controller::CapacityPool::open(1),
    );
    let mut holder = controller
        .clone()
        .acquire(
            node.clone(),
            pending_call("holder-call"),
            "recovery-cas",
            None,
            None,
        )
        .await
        .unwrap();
    let waiting_controller = controller.clone();
    let waiting_node = node.clone();
    let waiting = tokio::spawn(async move {
        waiting_controller
            .acquire(
                waiting_node,
                pending_call("waiting-call"),
                "recovery-cas",
                None,
                None,
            )
            .await
    });
    let queued = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(row) = load_stale_inference_calls(&node, NODE_DID)
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
    assert_eq!(
        (
            controller.pool.held_for_test(),
            controller.pool.queue_waiters_for_test()
        ),
        (0, 0),
        "losing admission must release controller bookkeeping"
    );
    let response = node
        .execute(r#"{ InferenceCall(filter: {call_id: {_eq: "waiting-call"}}) {call_state} }"#)
        .await;
    let rows = crate::store::graphql::rows::<Value>(&response, "InferenceCall").unwrap();
    assert_eq!(rows[0]["call_state"], "cancelled");
}

struct SignalledPermitFinalizer {
    permit: super::super::permit::AdmissionPermit,
    invoked: tokio::sync::oneshot::Sender<()>,
}

impl super::super::stream_guard::StreamGuardLifecycle for SignalledPermitFinalizer {
    fn mark_stream_success(
        &mut self,
        usage: Option<super::super::ProviderCallUsage>,
        cached_input_tokens: gents_loop::rig_compat::CachedInputTokensObservation,
    ) {
        super::super::stream_guard::StreamGuardLifecycle::mark_stream_success(
            &mut self.permit,
            usage,
            cached_input_tokens,
        );
    }

    fn mark_stream_error(&mut self, error: &str) {
        super::super::stream_guard::StreamGuardLifecycle::mark_stream_error(
            &mut self.permit,
            error,
        );
    }

    fn finish_stream(self) -> BoxFuture<'static, Result<(), super::super::AdmissionError>> {
        Box::pin(async move {
            let _ = self.invoked.send(());
            super::super::stream_guard::StreamGuardLifecycle::finish_stream(self.permit).await
        })
    }
}

/// Dropping a stream while its independently scheduled terminal persistence is
/// waiting for the canonical gate must still return real admission ownership.
/// Once the existing gate owner exits, AdmissionPermit's Drop repair installs
/// exactly one terminal outcome on the already-created call.
#[tokio::test]
async fn aborting_terminal_finalizer_returns_real_permit_and_repairs_call_once() {
    use super::super::controller::BackendAdmissionController;
    use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
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
        super::super::controller::CapacityPool::open(1),
    );
    let permit = controller
        .clone()
        .acquire(
            node.clone(),
            pending_call("aborted-finalizer-call"),
            "recovery-cas",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(controller.pool.available_permits_for_test(), 0);

    let gate_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let blocker_node = node.clone();
    let blocker_entered = gate_entered.clone();
    let blocker_release = release_gate.clone();
    let blocker = tokio::spawn(async move {
        crate::store::config_client::ConfigAccess::transact_local_write(
            &blocker_node,
            None,
            "test.admission.abort_finalizer_gate_blocker",
            move |_txn| {
                let entered = blocker_entered.clone();
                let release = blocker_release.clone();
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await
    });
    gate_entered.notified().await;

    let (invoked_tx, invoked_rx) = tokio::sync::oneshot::channel();
    let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Ok(
        RawStreamingChoice::FinalResponse(()),
    )])));
    let mut stream = crate::inference::llm::rig_compat::hold_stream_guard(
        inner,
        SignalledPermitFinalizer {
            permit,
            invoked: invoked_tx,
        },
    );
    let mut terminal = Box::pin(stream.next());
    tokio::select! {
        result = invoked_rx => result.expect("terminal finalizer must start"),
        item = &mut terminal => panic!("terminal item escaped blocked persistence: {item:?}"),
    }
    drop(terminal);
    drop(stream);

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while controller.pool.held_for_test() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aborted finalizer must drop its AdmissionPermit");
    assert_eq!(
        controller.pool.available_permits_for_test(),
        1,
        "aborted finalizer must return the real provider permit"
    );

    release_gate.notify_one();
    blocker
        .await
        .expect("gate blocker task")
        .expect("gate blocker transaction");

    let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = node
                .execute(
                    r#"{ InferenceCall(filter: {call_id: {_eq: "aborted-finalizer-call"}}) {call_state} }"#,
                )
                .await;
            let rows = crate::store::graphql::rows::<Value>(&response, "InferenceCall").unwrap();
            if rows.first().is_some_and(|row| row["call_state"] == "completed") {
                break rows;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("AdmissionPermit drop repair must persist a terminal outcome");
    assert_eq!(
        rows.len(),
        1,
        "drop repair must not create a second call row"
    );
    assert_eq!(rows[0]["call_state"], "completed");
}

/// Lean namespace InferenceCall.Persistence: terminal_winner_preserves_outcome_and_stamp and
/// late_usage_preserves_terminal_projection / late_usage_is_recorded.
#[tokio::test]
async fn recovery_winner_preserves_terminal_stamp_and_rehydrates_late_usage() {
    use crate::store::identity::NodeIdentity;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let keys = tempfile::tempdir().unwrap();
    let identity =
        crate::store::identity::KeyIdentity::load_or_create(keys.path().join("node.key"), None)
            .unwrap();
    crate::test_support::install_test_agent(&node, identity.did(), "test").await;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "recovery-cas-request",
        identity.did(),
        identity.did(),
        "test",
        "recovery-cas-session",
        "Resume the request",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    crate::sign_agent_request_create(&identity, &mut create)
        .await
        .unwrap();
    let access = crate::store::config_client::ConfigAccess::local(node.clone());
    let response = access
        .write("test.recovery_parent", &create.graphql_mutation().unwrap())
        .await
        .unwrap();
    let parent_doc_id = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let parent = crate::request::admission::load_request_for_admission_test(&node, &parent_doc_id)
        .await
        .unwrap();
    let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test",
        identity.did(),
        parent,
        120,
        crate::request::lifecycle::ExecutionOrigin::Interactive,
        "recovery-cas-backend",
    );
    lifecycle.set_configured_max_total_tokens(Some(100));
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let request = lifecycle.request().clone();
    let snapshot = seed_call(&node, "running", identity.did(), &request.doc_id).await;
    assert!(
        recover_inference_call_row(&node, &snapshot, InferenceRecoveryOutcome::Failed)
            .await
            .unwrap()
    );
    let terminal = stored_call(&node).await;
    let mut metadata = pending_call("recovery-cas-call");
    metadata.request_doc_id = request.doc_id.clone();
    metadata.node_did = request.node_did.clone();
    let call = super::super::controller::InferenceCallRecord::without_controller(metadata);
    let usage = super::super::ProviderCallUsage(gents_protocol::usage::TokenUsage {
        input_tokens: Some(17),
        output_tokens: Some(9),
        cached_input_tokens: Some(3),
        final_response: true,
        ..Default::default()
    });
    super::super::persistence::persist_existing_call_terminal(
        node.clone(),
        &call,
        &snapshot.doc_id,
        "completed",
        None,
        Some(usage),
        gents_loop::rig_compat::CachedInputTokensObservation::NotAvailable,
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
    let budget = crate::completion_factory::aggregate_token_budget_for_request(&node, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        budget.snapshot().unwrap().remaining(),
        74,
        "recovery must not discard observed usage on restart"
    );
    super::super::persistence::persist_existing_call_terminal(
        node.clone(),
        &call,
        &snapshot.doc_id,
        "completed",
        None,
        Some(usage),
        gents_loop::rig_compat::CachedInputTokensObservation::NotAvailable,
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
