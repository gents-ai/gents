use std::sync::Arc;

use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{
    submit_client_request, ClaimOutcome, ExecutionOrigin, RequestLifecycle,
};
use crate::session::message::{AgentMessageDelivery, Plan, RenderedBody};
use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::store::identity::NodeIdentity;
use crate::tools::call::lifecycle::{FailureClass, ToolCallLifecycle};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::SessionMessageCause;
use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_protocol::row::AgentRequest;
use gents_protocol::tool_call::AwaitMode;
use serde_json::json;

struct Fixture {
    _db: crate::support::TestDb,
    node: Arc<defra_node::Node>,
    access: ConfigAccess,
    config: ConfigAccess,
    parent_binding: gents_protocol::agent_identity::AgentIdentityBinding,
    runtime: Arc<dyn NodeIdentity>,
    parent: Arc<dyn NodeIdentity>,
    child: Arc<dyn NodeIdentity>,
    _owner: RequestLifecycle,
    cause: SessionMessageCause,
    tool: ToolCallLifecycle,
    plan: Option<Plan>,
}

async fn fixture(name: &str) -> Fixture {
    // Users write configuration through the signed client endpoint; a local
    // ConfigAccess may act only as the node or a managed Agent.
    let fixture_name = format!("finality-{name}");
    let db = crate::support::test_db(&fixture_name).await;
    let (db, user, user_access) =
        crate::test_support::signed_test_node_without_agent_from_db(db, &fixture_name).await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
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
                crate::documents::apply_desired_state_plan(txn, node_plan).await
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
    // Each Agent carries a context so the runtime projects its baseline node
    // grants; an Agent without one cannot read its own requests.
    let docs: Vec<_> = [("parent", &parent), ("child", &child)]
        .into_iter()
        .flat_map(|(name, binding)| {
            crate::test_support::test_agent_documents(runtime.did(), name, binding)
        })
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(docs).unwrap();
    user_access
        .transact_write("test.finality.agents", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
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
    let create = gents_protocol::request_admission::AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        runtime.did(),
        user.did(),
        parent.agent_did.clone(),
        session_id,
        "start child",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let mut owner =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &user_access,
            runtime.clone(),
            &user,
            parent_identity.as_ref(),
            create,
        )
        .await
        .unwrap();
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        parent.agent_did.parse().unwrap(),
        parent_identity.as_ref(),
    )
    .unwrap();
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
    let signer = parent_identity.managed_agent_signer().unwrap();
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
        &signer,
    )
    .await
    .unwrap()
    .unwrap();
    plan.sign_agent_creation(runtime.did(), &caller.input, &signer)
        .await
        .unwrap();
    Fixture {
        _db: db,
        node,
        access,
        config: user_access,
        parent_binding: parent,
        runtime,
        parent: parent_identity,
        child: child_identity,
        _owner: owner,
        cause,
        tool,
        plan: Some(plan),
    }
}

/// Enables or disables the parent Agent as its configuring User. The runtime
/// projects an Agent's baseline node grants from its live Agent document
/// (`GrantSet::AgentBaseline`), so disabling the document revokes them.
async fn set_parent_enabled(fixture: &Fixture, enabled: bool) {
    let (collection, mut value) = crate::test_support::test_agent_documents(
        fixture.runtime.did(),
        "parent",
        &fixture.parent_binding,
    )
    .into_iter()
    .find(|(collection, _)| matches!(collection, crate::Collection::Agent))
    .unwrap();
    value["enabled"] = json!(enabled);
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    fixture
        .config
        .transact_write("test.finality.parent_enabled", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
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
        crate::store::graphql::escape_graphql_string(doc), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
    )).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    crate::store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
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
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle
        .bind_managed_agent(fixture.child.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    fixture.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn existing_child_message_uses_calling_agent_native_claim() {
    let modeled = crate::lean_vocab_test::lean_contract_snapshot();
    let allowed = modeled
        .cross_actor_authorization_cases
        .iter()
        .find(|case| case.name == "committed_claim")
        .unwrap();
    let denied = modeled
        .cross_actor_authorization_cases
        .iter()
        .find(|case| case.name == "node_denied_at_check")
        .unwrap();
    assert_eq!(
        allowed.decision,
        crate::lean_vocab_test::LeanCrossActorDecision::Claimed
    );
    assert_eq!(
        denied.decision,
        crate::lean_vocab_test::LeanCrossActorDecision::Denied
    );
    for (active, deny) in [(false, false), (false, true), (true, true)] {
        let mut fixture = fixture(if active {
            "existing-active"
        } else if deny {
            "existing-idle-denied"
        } else {
            "existing-idle-allowed"
        })
        .await;
        let first = crate::session::message::commit(
            &fixture.node,
            &fixture.cause,
            &mut fixture.tool,
            fixture.plan.take().unwrap(),
            false,
        )
        .await
        .unwrap();
        let child_access = ConfigAccess::local_managed_agent(
            fixture.node.clone(),
            fixture.child.did().parse().unwrap(),
            fixture.child.as_ref(),
        )
        .unwrap();
        let query = format!(
            "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            crate::store::graphql::escape_graphql_string(&first.request_doc_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        );
        let response = child_access.execute(&query).await.unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let opening = AgentRequest::try_from(row).unwrap();
        let (_owner, authority) =
            crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(
            fixture.node.clone(),
            fixture.runtime.clone(),
            authority,
        );
        let (opening, admission) = verifier
            .verify_fresh_for_claim_with_identity(&opening, fixture.child.as_ref())
            .await
            .unwrap();
        assert!(admission.is_some());
        let mut child = RequestLifecycle::new_with_execution_binding(
            fixture.node.clone(),
            opening,
            60,
            ExecutionOrigin::Interactive,
            String::new(),
        );
        child.bind_managed_agent(fixture.child.as_ref()).unwrap();
        child.set_client_claim_admission(admission);
        child.set_steering_admission(verifier.clone(), fixture.child.clone());
        assert!(matches!(
            child.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        ));
        let writer = crate::request::streaming::DefraStreamWriter::new_managed(
            fixture.node.clone(),
            fixture.child.as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        child.begin_owned_execution(&writer).await.unwrap();
        if !active {
            child
                .terminalize_owned(
                    crate::request::lifecycle::RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("fixture completed its first turn"),
                )
                .await
                .unwrap();
        }
        let mut tool =
            crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut fixture._owner,
                fixture.runtime.did(),
                1,
                crate::tools::native::AGENT_MESSAGE_TOOL_NAME,
                "followup-tool",
                json!({"session_id":first.session_id.clone(),"message":"follow up"}),
                AwaitMode::Background,
                false,
            )
            .await
            .unwrap();
        let mut cause = fixture.cause.clone();
        cause.tool_call_id = tool.tool_call_id().to_owned();
        cause.tool_call_doc_id = tool.doc_id().unwrap().to_owned();
        let target = SessionMessageTarget {
            node_did: fixture.runtime.did().into(),
            agent_did: fixture.child.did().into(),
            session_id: first.session_id.clone(),
        };
        let signer = fixture.parent.managed_agent_signer().unwrap();
        if !active && !deny {
            let mut forged = cause.clone();
            forged.tool_call_doc_id = "unrelated-tool-document".into();
            let forged_plan = crate::session::message::plan_with_delivery(
                &fixture.node,
                &forged,
                &target,
                RenderedBody {
                    content: "forged".into(),
                    goal: None,
                },
                None,
                AgentMessageDelivery::Queue,
                &signer,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(crate::session::message::commit(
                &fixture.node,
                &forged,
                &mut tool,
                forged_plan,
                true,
            )
            .await
            .is_err());
            let rows = fixture.access.execute(
                "{ AgentRequest(filter: { caused_by_parent_tool_call_doc_id: { _eq: \"unrelated-tool-document\" } }) { _docID } }",
            ).await.unwrap();
            assert!(rows["data"]["AgentRequest"].as_array().unwrap().is_empty());
        }
        let plan = crate::session::message::plan_with_delivery(
            &fixture.node,
            &cause,
            &target,
            RenderedBody {
                content: "follow up".into(),
                goal: None,
            },
            None,
            if active {
                AgentMessageDelivery::Steer
            } else {
                AgentMessageDelivery::Queue
            },
            &signer,
        )
        .await
        .unwrap()
        .unwrap();
        let receipt = crate::session::message::commit(&fixture.node, &cause, &mut tool, plan, true)
            .await
            .unwrap();
        let query = format!(
            "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            crate::store::graphql::escape_graphql_string(&receipt.request_doc_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        );
        let row = child_access.execute(&query).await.unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row["data"]["AgentRequest"][0].clone()).unwrap();
        assert_eq!(row.admission_kind.as_deref(), Some("client"));
        assert_eq!(
            row.admission_signer_did.as_deref(),
            Some(fixture.parent.did())
        );
        assert_eq!(row.requester_did.as_deref(), Some(fixture.parent.did()));
        assert_eq!(row.agent_did.as_deref(), Some(fixture.child.did()));
        assert!(row
            .session_doc_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        assert!(row
            .agent_doc_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        let session_doc_id = row.session_doc_id.clone();
        let agent_doc_id = row.agent_doc_id.clone();
        crate::store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
        let request = AgentRequest::try_from(row).unwrap();
        let (request, admission) = verifier
            .verify_fresh_for_claim_with_identity(&request, fixture.child.as_ref())
            .await
            .unwrap();
        assert!(admission.is_some());
        if deny {
            set_parent_enabled(&fixture, false).await;
        }
        if active {
            // A revoked parent can no longer prove the message source, so the
            // steering snapshot fails closed instead of omitting the input.
            let blocked = crate::request::lifecycle::queue::steering_snapshot(
                &fixture.node,
                child.request(),
                &verifier,
                Some(fixture.child.as_ref()),
            )
            .await;
            let error = blocked
                .err()
                .expect("a revoked parent's steering snapshot fails closed");
            assert!(
                format!("{error:#}").contains("local agent-message source is unavailable"),
                "{error:#}"
            );
            set_parent_enabled(&fixture, true).await;
            let snapshot = crate::request::lifecycle::queue::steering_snapshot(
                &fixture.node,
                child.request(),
                &verifier,
                Some(fixture.child.as_ref()),
            )
            .await
            .unwrap();
            assert_eq!(snapshot.inputs.len(), 1);
            writer
                .publish_authored_message(
                    &child,
                    &snapshot.inputs[0].key,
                    &snapshot.inputs[0].message,
                )
                .await
                .unwrap();
            let observed = child_access.execute(&query).await.unwrap();
            assert_eq!(
                observed["data"]["AgentRequest"][0]["lifecycle_state"],
                "superseded"
            );
        } else {
            let mut later = RequestLifecycle::new_with_execution_binding(
                fixture.node.clone(),
                request,
                60,
                ExecutionOrigin::Interactive,
                String::new(),
            );
            later.bind_managed_agent(fixture.child.as_ref()).unwrap();
            later.set_client_claim_admission(admission);
            let outcome = later.claim_with_identity().await;
            if deny {
                assert!(outcome.is_err(), "revoked immediate Agent cannot claim");
            } else {
                assert!(matches!(outcome.unwrap(), ClaimOutcome::Claimed));
            }
            let observed = child_access.execute(&query).await.unwrap();
            assert_eq!(
                observed["data"]["AgentRequest"][0]["lifecycle_state"],
                if deny { "failed" } else { "claimed" }
            );
        }
        if !active && !deny {
            let mut forged = gents_protocol::request_admission::AgentRequestCreate::base(
                RequestPurpose::Normal,
                "forged-source",
                fixture.runtime.did(),
                fixture.parent.did(),
                fixture.child.did(),
                &first.session_id,
                "forged source",
                "interactive",
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                AgentRequestAdmissionRecord::client(fixture.parent.did()),
            );
            forged.session_doc_id = session_doc_id;
            forged.agent_doc_id = agent_doc_id;
            forged.caused_by_parent_request_id = Some(cause.caller_request_id.clone());
            forged.caused_by_parent_request_doc_id = Some(cause.caller_request_doc_id.clone());
            forged.caused_by_parent_tool_call_id = Some(cause.tool_call_id.clone());
            forged.caused_by_parent_tool_call_doc_id = Some("unrelated-tool-document".into());
            forged.request_hop = cause.caller_hop + 1;
            crate::sign_agent_request_create(fixture.parent.as_ref(), &mut forged)
                .await
                .unwrap();
            let parent_access = ConfigAccess::local_managed_agent(
                fixture.node.clone(),
                fixture.parent.did().parse().unwrap(),
                fixture.parent.as_ref(),
            )
            .unwrap();
            submit_client_request(&parent_access, &forged)
                .await
                .unwrap();
            let response = child_access.execute(&format!(
                "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"forged-source\" }} }}) {{ {} }} }}",
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            )).await.unwrap();
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
            let forged = AgentRequest::try_from(row).unwrap();
            let verdict = verifier
                .verify_fresh_for_claim_with_identity(&forged, fixture.child.as_ref())
                .await;
            assert!(
                verdict.is_err(),
                "a signed request with an unrelated source call must not receive a claim witness: {verdict:?}"
            );
        }
        fixture.node.shutdown().await.unwrap();
    }
}
