use super::*;
use futures::{future::BoxFuture, StreamExt};
use serde_json::Value;

fn agent_access(
    node: &std::sync::Arc<EmbeddedNode>,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> crate::ConfigAccess {
    crate::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(signer.did()).unwrap(),
        signer.clone(),
    )
    .unwrap()
}

async fn agent_query(
    node: &std::sync::Arc<EmbeddedNode>,
    signer: &crate::store::identity::ManagedAgentSigner,
    document: &str,
) -> query::QueryResponse {
    let access = agent_access(node, signer);
    let value = if document.trim_start().starts_with("mutation") {
        access.write("test.inference_recovery", document).await
    } else {
        access.execute(document).await
    }
    .unwrap();
    serde_json::from_value(value).unwrap()
}

async fn fixture(
    initial_state: &str,
) -> (
    crate::support::TestDb,
    crate::store::identity::ManagedAgentSigner,
    StaleInferenceCallRow,
) {
    let (db, signer) = super::super::tests::test_node().await;
    let node = db.node.clone();
    let snapshot = seed_call(&node, &signer, initial_state, "recovery-cas-request-doc").await;
    (db, signer, snapshot)
}

async fn seed_call(
    node: &std::sync::Arc<EmbeddedNode>,
    signer: &crate::store::identity::ManagedAgentSigner,
    initial_state: &str,
    request_doc_id: &str,
) -> StaleInferenceCallRow {
    let agent_did = escape_graphql_string(signer.did());
    let state = escape_graphql_string(initial_state);
    let owner = escape_graphql_string(signer.node_did());
    let request_doc = escape_graphql_string(request_doc_id);
    let access = agent_access(node, signer);
    access
        .write(
            "test.recovery_call",
            &format!(
                r#"mutation {{ add_InferenceCall(input: {{
            call_id: "recovery-cas-call", runtime_instance_id: "recovery-cas-runtime",
            request_id: "recovery-cas-request", request_doc_id: "{request_doc}", call_seq: 1,
            backend_id: "recovery-cas-backend", agent_did: "{agent_did}", node_did: "{owner}",
            call_kind: "inference", attempt: 1, call_state: "{state}",
            queued_at: "2026-09-01T00:00:00Z", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .unwrap();
    let mut snapshots = load_stale_inference_calls(&access, signer.node_did(), signer.did())
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    snapshots.remove(0)
}

async fn stored_call(
    node: &std::sync::Arc<EmbeddedNode>,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Value {
    let value = agent_access(node, signer).execute(
        r#"{ InferenceCall { call_state failure_reason ended_at prompt_tokens completion_tokens } }"#,
    ).await.unwrap();
    let response: query::QueryResponse = serde_json::from_value(value).unwrap();
    crate::store::graphql::rows::<Value>(&response, "InferenceCall")
        .unwrap()
        .remove(0)
}

/// Lean InferenceCall's terminal states have no outgoing transitions. A sweep
/// that observed running before the owner completed must preserve that outcome.
#[tokio::test]
async fn stale_recovery_preserves_concurrent_completion_and_usage() {
    let (db, signer, snapshot) = fixture("running").await;
    let node = db.node.clone();
    let doc_id = escape_graphql_string(&snapshot.doc_id);
    let response = agent_query(
        &node,
        &signer,
        &format!(
            r#"mutation {{ update_InferenceCall(
            filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
            input: {{ call_state: "completed", ended_at: "2026-09-02T00:00:00Z",
                prompt_tokens: 17, completion_tokens: 9 }}
        ) {{ _docID }} }}"#
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let completed = stored_call(&node, &signer).await;
    assert!(!recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Failed
    )
    .await
    .unwrap());
    assert_eq!(stored_call(&node, &signer).await, completed);
}

#[tokio::test]
async fn competing_recovery_counts_only_the_winner_and_preserves_terminal_stamp() {
    let (db, signer, snapshot) = fixture("running").await;
    let node = db.node.clone();
    let winner = recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Failed,
    )
    .await
    .unwrap();
    assert!(winner);
    let terminal = stored_call(&node, &signer).await;
    assert_eq!(terminal["call_state"], "failed");
    let loser = recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Failed,
    )
    .await
    .unwrap();
    assert!(!loser);
    assert_eq!(usize::from(winner) + usize::from(loser), 1);
    assert_eq!(stored_call(&node, &signer).await, terminal);
}

#[tokio::test]
async fn stale_queued_recovery_cannot_cancel_a_running_call() {
    let (db, signer, snapshot) = fixture("queued").await;
    let node = db.node.clone();
    let doc_id = escape_graphql_string(&snapshot.doc_id);
    let response = agent_query(
        &node,
        &signer,
        &format!(
            r#"mutation {{ update_InferenceCall(
            filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
            input: {{ call_state: "running" }}
        ) {{ _docID }} }}"#
        ),
    )
    .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let running = stored_call(&node, &signer).await;
    assert!(!recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Cancelled
    )
    .await
    .unwrap());
    assert_eq!(stored_call(&node, &signer).await, running);
}

fn pending_call(
    call_id: &str,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> super::super::controller::PendingCallMetadata {
    super::super::controller::PendingCallMetadata {
        agent_signer: Some(signer.clone()),
        session_id: "session-test".to_string(),
        requester_did: None,
        request_purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        model_name: None,
        profile_id: None,
        call_id: call_id.into(),
        runtime_instance_id: "recovery-cas-runtime".into(),
        request_id: "recovery-cas-request".into(),
        request_doc_id: "recovery-cas-request-doc".into(),
        call_seq: 1,
        backend_id: "recovery-cas-backend".into(),
        agent_did: signer.did().into(),
        node_did: signer.node_did().into(),
        call_kind: super::super::CallKind::Inference,
        attempt: 1,
    }
}

async fn seeded_pending_call(
    node: &std::sync::Arc<EmbeddedNode>,
    signer: &crate::store::identity::ManagedAgentSigner,
    call_id: &str,
) -> super::super::controller::PendingCallMetadata {
    let request =
        super::super::tests::seeded_request(node, signer, &format!("request-{call_id}")).await;
    let mut metadata = pending_call(call_id, signer);
    metadata.request_id = request.request_id;
    metadata.request_doc_id = request.doc_id;
    metadata.session_id = request.session_id;
    metadata.requester_did = request.requester_did;
    metadata
}

/// Lean: InferenceCall.Persistence.recovery_winner_cannot_be_reopened.
#[tokio::test]
async fn recovery_winner_rejects_late_running_persistence() {
    let (db, signer, snapshot) = fixture("queued").await;
    let node = db.node.clone();
    assert!(recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Cancelled
    )
    .await
    .unwrap());
    let terminal = stored_call(&node, &signer).await;
    let call = super::super::controller::InferenceCallRecord::without_controller(pending_call(
        "recovery-cas-call",
        &signer,
    ));
    assert!(super::super::persistence::persist_existing_call_running(
        node.clone(),
        &call,
        &snapshot.doc_id,
    )
    .await
    .is_err());
    assert_eq!(stored_call(&node, &signer).await, terminal);
}

#[tokio::test]
async fn physical_call_updates_preserve_identity_and_state_predicates() {
    use super::super::controller::InferenceCallRecord;
    use super::super::persistence::{
        persist_call_queued, persist_existing_call_running, persist_existing_call_terminal,
    };
    use gents_loop::rig_compat::CachedInputTokensObservation;

    let (db, signer, snapshot) = fixture("queued").await;
    let node = db.node.clone();
    let call = InferenceCallRecord::without_controller(pending_call("recovery-cas-call", &signer));
    let other = InferenceCallRecord::without_controller(
        seeded_pending_call(&node, &signer, "other-call").await,
    );
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
    let response = agent_access(&node, &signer)
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
    let (db, signer) = super::super::tests::test_node().await;
    let node = db.node.clone();
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
            seeded_pending_call(&node, &signer, "holder-call").await,
            "recovery-cas",
            None,
            None,
        )
        .await
        .unwrap();
    let waiting_controller = controller.clone();
    let waiting_node = node.clone();
    let waiting_signer = signer.clone();
    let waiting = tokio::spawn(async move {
        let pending = seeded_pending_call(&waiting_node, &waiting_signer, "waiting-call").await;
        waiting_controller
            .acquire(waiting_node, pending, "recovery-cas", None, None)
            .await
    });
    let queued = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(row) = load_stale_inference_calls(
                &agent_access(&node, &signer),
                signer.node_did(),
                signer.did(),
            )
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
    assert!(recover_inference_call_row(
        &agent_access(&node, &signer),
        &queued,
        InferenceRecoveryOutcome::Cancelled
    )
    .await
    .unwrap());
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
    let response = agent_query(
        &node,
        &signer,
        r#"{ InferenceCall(filter: {call_id: {_eq: "waiting-call"}}) {call_state} }"#,
    )
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

    let (db, signer) = super::super::tests::test_node().await;
    let node = db.node.clone();
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
            seeded_pending_call(&node, &signer, "aborted-finalizer-call").await,
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
            let response = agent_query(&node, &signer,
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
    use crate::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
    use crate::store::identity::NodeIdentity;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};

    let (db, user, access, binding) =
        crate::test_support::signed_test_node("inference-recovery-parent").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let agent =
        crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    super::super::tests::grant_test_agent(&db, &signer).await;
    let request_id = "recovery-cas-request";
    let create = crate::request::lifecycle::build_signed_client_request(
        &access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.into(),
                node_did: identity.did().to_owned(),
                requester_did: Some(user.did().to_owned()),
                agent_did: agent.did().to_owned(),
                session_id: "recovery-cas-session".into(),
                content: "Resume the request".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&access, &create)
        .await
        .unwrap();
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(request_id),
        ))
        .await
        .unwrap();
    let parent_doc_id = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    let parent = crate::request::admission::load_request_for_admission_test_as(
        &node,
        parent_doc_id,
        agent.did(),
    )
    .await
    .unwrap();
    let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        identity.did(),
        parent,
        120,
        crate::request::lifecycle::ExecutionOrigin::Interactive,
        "recovery-cas-backend",
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_configured_max_total_tokens(Some(100));
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let request = lifecycle.request().clone();
    let snapshot = seed_call(&node, &signer, "running", &request.doc_id).await;
    assert!(recover_inference_call_row(
        &agent_access(&node, &signer),
        &snapshot,
        InferenceRecoveryOutcome::Failed
    )
    .await
    .unwrap());
    let terminal = stored_call(&node, &signer).await;
    let mut metadata = pending_call("recovery-cas-call", &signer);
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
    let stored = stored_call(&node, &signer).await;
    for field in ["call_state", "failure_reason", "ended_at"] {
        assert_eq!(
            stored[field], terminal[field],
            "late completion changed terminal {field}"
        );
    }
    assert_eq!(stored["prompt_tokens"], 17);
    assert_eq!(stored["completion_tokens"], 9);
    let budget = crate::turn::completion_factory::aggregate_token_budget_for_request(
        &agent_access(&node, &signer),
        &request,
    )
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
    assert_eq!(stored_call(&node, &signer).await, stored);
    let budget = crate::turn::completion_factory::aggregate_token_budget_for_request(
        &agent_access(&node, &signer),
        &request,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(budget.snapshot().unwrap().remaining(), 74);
}

#[tokio::test]
async fn parent_recovery_lookup_uses_agent_native_authority() {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let runtime = KeyIdentity::load_or_create(dir.path().join("runtime.key"), None).unwrap();
    let user = KeyIdentity::load_or_create(dir.path().join("user.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [user.did(), agent.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let owner = ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
    let created = owner.write("test.inference_recovery.request", &format!(
        r#"mutation {{ create_AgentRequest(input: {{ request_id: "private-recovery", node_did: "{}", agent_did: "{}", lifecycle_state: "{}" }}) {{ _docID }} }}"#,
        runtime.did(), agent.did(), RequestLifecycleState::Processing.as_str())).await.unwrap();
    let request_doc = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    owner
        .add_actor_relationship(
            "AgentRequest",
            &request_doc,
            "working_agent",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let row = StaleInferenceCallRow {
        doc_id: "call-doc".into(),
        call_id: "call".into(),
        request_id: "private-recovery".into(),
        request_doc_id: request_doc,
        agent_did: agent.did().into(),
        call_state: "running".into(),
    };
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let found = lookup_parent_request(&agent_access, runtime.did(), &row)
        .await
        .unwrap_or_else(|error| panic!("authorized parent lookup failed: {error:#}"));
    let parent = found.unwrap();
    assert_eq!(parent.request_id, "private-recovery");
    assert!(recovery_outcome(&row, &parent).is_none());
    owner.write("test.inference_recovery.terminal", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "{}" }}) {{ _docID }} }}"#,
        row.request_doc_id, RequestLifecycleState::Failed.as_str())).await.unwrap();
    let terminal = lookup_parent_request(&agent_access, runtime.did(), &row)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovery_outcome(&row, &terminal),
        Some(InferenceRecoveryOutcome::Failed)
    ));
    admin
        .revoke_node_permission(&agent.did().parse().unwrap(), NodePermission::DocumentRead)
        .await
        .unwrap();
    assert!(
        lookup_parent_request(&agent_access, runtime.did(), &row)
            .await
            .is_err(),
        "revoked agent must not look like an absent parent"
    );
    node.shutdown().await.unwrap();
}
