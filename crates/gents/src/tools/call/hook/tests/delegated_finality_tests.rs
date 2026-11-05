use std::sync::Arc;

use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{
    build_signed_client_request, submit_client_request, ClaimOutcome, ExecutionOrigin,
    RequestIdentity, RequestLifecycle, RequestSigner, RequestSpec, SessionMessageCause,
    SessionMessageTarget,
};
use crate::request::watcher::AgentRequest;
use crate::session::message::{AgentMessageDelivery, Plan, RenderedBody};
use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan, NodePermission,
};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use crate::tools::call::lifecycle::{AwaitMode, FailureClass, ToolCallLifecycle};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use serde_json::json;

struct Fixture {
    _home: tempfile::TempDir,
    node: Arc<defra_node::Node>,
    access: ConfigAccess,
    runtime: Arc<dyn NodeIdentity>,
    parent: Arc<dyn NodeIdentity>,
    child: Arc<dyn NodeIdentity>,
    _owner: RequestLifecycle,
    cause: SessionMessageCause,
    tool: ToolCallLifecycle,
    plan: Option<Plan>,
}

async fn fixture(name: &str) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let runtime: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        admin
            .grant_node_permission(&user.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    let user_access = ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
    let value = json!({"node_did": runtime.did(), "enabled": true});
    let node_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::Collection::Node,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    user_access
        .transact_write("test.finality.node", |txn| {
            let node_plan = &node_plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, node_plan).await
            })
        })
        .await
        .unwrap();
    let user_did = user.did().parse().unwrap();
    let parent = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    let child = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    let parent_identity =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &parent).unwrap();
    let child_identity =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &child).unwrap();
    let mut docs: Vec<_> = [
        (
            crate::Collection::InferenceBackend,
            json!({
                "node_did": runtime.did(), "backend_id": "fixture", "name": "Fixture",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"}
            }),
        ),
        (
            crate::Collection::InferenceProfile,
            json!({
                "node_did": runtime.did(), "profile_id": "fixture", "backend_id": "fixture",
                "model_name": "fixture"
            }),
        ),
    ]
    .into_iter()
    .map(|(collection, value)| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
    .collect();
    docs.extend([&parent, &child].into_iter().map(|binding| {
        let value = json!({"node_did": runtime.did(), "agent_did": binding.agent_did,
            "creation_nonce": binding.creation_nonce, "inference_profile_id": "fixture", "enabled": true});
        DesiredStateApplyDocument { collection: crate::Collection::Agent, add: value.clone(), update: value }
    }));
    let plan = DesiredStateApplyPlan::new(docs).unwrap();
    user_access
        .transact_write("test.finality.agents", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    let agents = user_access
        .execute("{ Agent { _docID agent_did } }")
        .await
        .unwrap();
    for binding in [&parent, &child] {
        let row = agents["data"]["Agent"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["agent_did"] == binding.agent_did)
            .unwrap();
        let doc = row["_docID"].as_str().unwrap();
        user_access
            .add_actor_relationship(
                "Agent",
                doc,
                "executor",
                &binding.agent_did.parse().unwrap(),
            )
            .await
            .unwrap();
        if binding.agent_did == child.agent_did {
            user_access
                .add_actor_relationship(
                    "Agent",
                    doc,
                    "session_writer",
                    &parent.agent_did.parse().unwrap(),
                )
                .await
                .unwrap();
        }
    }
    let request_id = format!("finality-{name}");
    let session_id = format!("finality-session-{name}");
    let create = build_signed_client_request(
        &user_access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id,
                node_did: runtime.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: parent.agent_did.clone(),
                session_id,
                content: "start child".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    submit_client_request(&user_access, &create).await.unwrap();
    let access = ConfigAccess::local_as(node.clone(), parent.agent_did.parse().unwrap());
    let response = access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, &parent.agent_did)
        .await
        .unwrap();
    let mut owner = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        &parent.agent_did,
        runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    owner.set_client_claim_admission(admission);
    assert!(matches!(
        owner.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    let tool =
        crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut owner,
            runtime.did(),
            0,
            crate::tools::native::AGENT_NEW_TOOL_NAME,
            "finality-tool",
            json!({"agent":"child", "prompt":"work"}),
            AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
    let caller = owner.request();
    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
        correlation: caller.caused_by_correlation.clone(),
    };
    let target = SessionMessageTarget {
        node_did: runtime.did().into(),
        agent_did: child.agent_did,
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut plan = crate::session::message::plan_with_delivery(
        &node,
        &cause,
        &target,
        RenderedBody {
            content: "work".into(),
            goal: None,
        },
        None,
        AgentMessageDelivery::Queue,
    )
    .await
    .unwrap()
    .unwrap();
    plan.sign_agent_creation(runtime.did(), &caller.input)
        .await
        .unwrap();
    Fixture {
        _home: home,
        node,
        access,
        runtime,
        parent: parent_identity,
        child: child_identity,
        _owner: owner,
        cause,
        tool,
        plan: Some(plan),
    }
}

async fn observed(fixture: &Fixture) -> (String, Vec<serde_json::Value>) {
    let tool = crate::store::graphql::escape_graphql_string(&fixture.cause.tool_call_doc_id);
    let response = fixture.access.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }} }}, limit: 2) {{ lifecycle_state }} AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{tool}" }} }}) {{ _docID request_id session_id }} }}"#,
    )).await.unwrap();
    (
        response["data"]["AgentToolCall"][0]["lifecycle_state"]
            .as_str()
            .unwrap()
            .to_owned(),
        response["data"]["AgentRequest"].as_array().unwrap().clone(),
    )
}

fn model(name: &str) -> serde_json::Value {
    crate::lean_vocab_test::lean_contract_snapshot()
        .delegated_finality_cases
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn competing_failure_and_child_commit_obey_the_native_compare() {
    for failure_first in [true, false] {
        let mut fixture = fixture(if failure_first {
            "failure-first"
        } else {
            "child-first"
        })
        .await;
        let expected = model(if failure_first {
            "failure-wins-before-child"
        } else {
            "child-wins-after-pending-read"
        });
        let mut child_tool = fixture.tool.clone();
        let mut competitor = fixture.tool.clone();
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut child = Box::pin(ConfigApplyTxn::with_embedded_before_commit_pause(
            reached.clone(),
            release.clone(),
            crate::session::message::commit(
                &fixture.node,
                &fixture.cause,
                &mut child_tool,
                fixture.plan.take().unwrap(),
                false,
            ),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = reached.notified() => {}
                result = &mut child => panic!("child did not reach native commit boundary: {result:?}"),
            }
        }).await.expect("child reaches its native commit boundary");
        let (state, children) = observed(&fixture).await;
        assert_eq!(state, "pending");
        assert!(children.is_empty());
        if failure_first {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                competitor.spawn_failed(FailureClass::External, "competing failure"),
            )
            .await
            .expect("competing failure completes while the child commit is held")
            .unwrap();
            release.notify_one();
            assert!(child.await.is_err());
            let (state, children) = observed(&fixture).await;
            assert_eq!(expected["expected_native"], "failure_committed");
            assert_eq!(state, "failed");
            assert!(children.is_empty());
        } else {
            release.notify_one();
            let receipt = child.await.unwrap();
            assert!(competitor
                .spawn_failed(FailureClass::External, "competing failure")
                .await
                .is_err());
            let (state, children) = observed(&fixture).await;
            assert_eq!(expected["expected_native"], "child_committed");
            assert_eq!(state, "running");
            assert_eq!(children.len(), 1);
            assert_eq!(children[0]["_docID"], receipt.request_doc_id);
        }
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn lost_commit_ack_recovers_the_exact_child_receipt() {
    let mut fixture = fixture("lost-ack").await;
    let expected = model("child-committed-ack-lost");
    let mut tool = fixture.tool.clone();
    let (result, fired) = ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
        Some("session_message.commit_request"),
        crate::session::message::commit(
            &fixture.node,
            &fixture.cause,
            &mut tool,
            fixture.plan.take().unwrap(),
            false,
        ),
    )
    .await;
    assert!(
        fired,
        "the transaction must lose its post-commit acknowledgement"
    );
    let receipt = result.unwrap();
    let (state, children) = observed(&fixture).await;
    assert_eq!(expected["expected_knowledge"], "confirmed");
    assert_eq!(state, "running");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["_docID"], receipt.request_doc_id);
    assert_eq!(children[0]["request_id"], receipt.request_id);
    assert_eq!(children[0]["session_id"], receipt.session_id);
    fixture.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_after_child_commit_recovers_relationships_in_the_actor_watcher() {
    let mut fixture = fixture("cancelled-after-commit").await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let mut tool = fixture.tool.clone();
    let mut dispatch = Box::pin(ConfigApplyTxn::with_embedded_commit_ack_pause(
        reached.clone(),
        crate::session::message::commit(
            &fixture.node,
            &fixture.cause,
            &mut tool,
            fixture.plan.take().unwrap(),
            false,
        ),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = reached.notified() => {}
            result = &mut dispatch => panic!("dispatch returned before cancellation: {result:?}"),
        }
    })
    .await
    .expect("child publication reaches the committed acknowledgement boundary");
    drop(dispatch);
    let (state, children) = observed(&fixture).await;
    assert_eq!(state, "running");
    assert_eq!(children.len(), 1);
    let doc = children[0]["_docID"].as_str().unwrap();
    let child_access =
        ConfigAccess::local_as(fixture.node.clone(), fixture.child.did().parse().unwrap());
    assert!(child_access
        .execute("{ AgentRequest { _docID } }")
        .await
        .unwrap()["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());

    let watcher = crate::request::watcher::DefraWatcher::for_agents(
        fixture.node.clone(),
        fixture.runtime.did(),
        vec![fixture.parent.clone(), fixture.child.clone()],
    )
    .unwrap();
    // The child may sort before the parent; a subsequent ordinary scan sees the recovered grant.
    let _ = watcher.pending_requests().await.unwrap();
    let requests = watcher.pending_requests().await.unwrap();
    let request = requests
        .into_iter()
        .find(|request| request.doc_id == doc)
        .expect("child sees its published opening");
    assert_eq!(request.agent_did, fixture.child.did());
    assert_eq!(request.requester_did.as_deref(), Some(fixture.parent.did()));
    let response = child_access.execute(&format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} relationships_pending }} }}",
        crate::store::graphql::escape_graphql_string(doc), crate::SIGNED_REQUEST_FIELDS,
    )).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    crate::request::admission::verify_request_receipt_signature(&row).unwrap();
    assert_eq!(
        row.admission_signer_did.as_deref(),
        Some(fixture.parent.did())
    );
    assert_eq!(
        response["data"]["AgentRequest"][0]["relationships_pending"],
        false
    );
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(
        fixture.node.clone(),
        fixture.runtime.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, fixture.child.did())
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        fixture.node.clone(),
        fixture.child.did(),
        fixture.runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.set_client_claim_admission(admission);
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    fixture.node.shutdown().await.unwrap();
}
