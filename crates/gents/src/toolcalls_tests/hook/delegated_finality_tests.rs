use std::sync::Arc;

use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::SessionMessageCause;
use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_protocol::row::AgentRequest;
use gents_protocol::tool_call::AwaitMode;
use gents_session::request::admission::AgentRequestAdmissionVerifier;
use gents_session::request::lifecycle::{
    submit_client_request, ClaimOutcome, ExecutionOrigin, RequestLifecycle,
};
use gents_session::session::message::{AgentMessageDelivery, Plan, RenderedBody};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
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
        crate::test_support::root::signed_test_node_without_agent_from_db(db, &fixture_name).await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let value = json!({"node_did": runtime.did(), "enabled": true});
    let node_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Node,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    user_access
        .transact_write("test.finality.node", |txn| {
            let node_plan = &node_plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    node_plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let user_did = user.did().parse().unwrap();
    let parent = gents_store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    let child = gents_store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    let parent_identity =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &parent).unwrap();
    let child_identity =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &child).unwrap();
    // Each Agent carries a context so the runtime projects its baseline node
    // grants; an Agent without one cannot read its own requests.
    let docs: Vec<_> = [("parent", &parent), ("child", &child)]
        .into_iter()
        .flat_map(|(name, binding)| {
            gents_documents::test_support::test_agent_documents(runtime.did(), name, binding)
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
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
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
        gents_toolcalls::call::lifecycle::admission_fixture::pending_client_fixture_request(
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
        gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut owner,
            runtime.did(),
            0,
            gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
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
    let mut plan = gents_session::session::message::plan_with_delivery(
        &node,
        &gents_automation::goal::session_message::SessionGoals,
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
    let (collection, mut value) = gents_documents::test_support::test_agent_documents(
        fixture.runtime.did(),
        "parent",
        &fixture.parent_binding,
    )
    .into_iter()
    .find(|(collection, _)| matches!(collection, gents_store::collection::Collection::Agent))
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
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

async fn observed(fixture: &Fixture) -> (String, Vec<serde_json::Value>) {
    let tool = gents_store::graphql::escape_graphql_string(&fixture.cause.tool_call_doc_id);
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

fn native(state: &str, children: &[serde_json::Value], receipt: Option<&str>) -> String {
    match (state, children, receipt) {
        ("pending", [], _) => "pending".into(),
        ("failed", [], _) => "failure_committed".into(),
        ("running", [child], Some(receipt)) if child["_docID"] == receipt => {
            "child_committed".into()
        }
        other => panic!("unmodeled native observation {other:?}"),
    }
}

#[tokio::test]
async fn delegated_commit_finality_follows_the_lean_traces() {
    for case in
        gents_lean_vocab::families::delegated_finality_served::cases::<String, String, String>()
    {
        let name = case.input.name.as_str();
        let mut fixture = fixture(name).await;
        let mut child_tool = fixture.tool.clone();
        let observation = if name == "child-committed-ack-lost" {
            let (result, fired) = ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                Some("session_message.commit_request"),
                gents_session::session::message::commit(
                    &fixture.node,
                    &gents_automation::goal::session_message::SessionGoals,
                    &fixture.cause,
                    &mut child_tool,
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
            assert_eq!(children.len(), 1);
            assert_eq!(children[0]["request_id"], receipt.request_id);
            assert_eq!(children[0]["session_id"], receipt.session_id);
            (
                native(&state, &children, Some(&receipt.request_doc_id)),
                "confirmed",
            )
        } else {
            let failure_first = name == "failure-wins-before-child";
            let mut competitor = fixture.tool.clone();
            let reached = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let mut child = Box::pin(ConfigApplyTxn::with_embedded_before_commit_pause(
                reached.clone(),
                release.clone(),
                gents_session::session::message::commit(
                    &fixture.node,
                    &gents_automation::goal::session_message::SessionGoals,
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
            assert_eq!(native(&state, &children, None), "pending");
            if failure_first {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    competitor.spawn_failed(FailureClass::External, "competing failure"),
                )
                .await
                .expect("competing failure completes while the child commit is held")
                .unwrap();
                release.notify_one();
                let outcome = child.await;
                let (state, children) = observed(&fixture).await;
                (
                    native(&state, &children, None),
                    if outcome.is_err() {
                        "aborted"
                    } else {
                        "confirmed"
                    },
                )
            } else {
                release.notify_one();
                let receipt = child.await.unwrap();
                assert!(competitor
                    .spawn_failed(FailureClass::External, "competing failure")
                    .await
                    .is_err());
                let (state, children) = observed(&fixture).await;
                (
                    native(&state, &children, Some(&receipt.request_doc_id)),
                    "confirmed",
                )
            }
        };
        fixture.node.shutdown().await.unwrap();
        case.check(
            gents_lean_vocab::families::delegated_finality_served::Finality {
                native: observation.0,
                knowledge: observation.1.to_string(),
            },
        );
    }
}

#[tokio::test]
async fn dropped_commit_ack_recovery_matches_the_lean_model() {
    for case in gents_lean_vocab::families::delegated_publication_recovery_watcher::cases() {
        let mut fixture = fixture(&case.input.name).await;
        let reached = Arc::new(tokio::sync::Notify::new());
        let mut tool = fixture.tool.clone();
        let mut dispatch = Box::pin(ConfigApplyTxn::with_embedded_commit_ack_pause(
            reached.clone(),
            gents_session::session::message::commit(
                &fixture.node,
                &gents_automation::goal::session_message::SessionGoals,
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
        let request_committed = children.len() == 1;
        let doc = children
            .first()
            .and_then(|child| child["_docID"].as_str())
            .unwrap_or_default();
        let child_access =
            ConfigAccess::local_as(fixture.node.clone(), fixture.child.did().parse().unwrap());
        let child_read_before_watcher = !child_access
            .execute("{ AgentRequest { _docID } }")
            .await
            .unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty();

        let watcher = gents_session::request::watcher::DefraWatcher::for_agents(
            fixture.node.clone(),
            fixture.runtime.did(),
            vec![fixture.parent.clone(), fixture.child.clone()],
        )
        .unwrap();
        let _ = watcher.pending_requests().await.unwrap();
        let requests = watcher.pending_requests().await.unwrap();
        let child_read_after_watcher = requests.iter().any(|request| {
            request.doc_id == doc
                && request.agent_did == fixture.child.did()
                && request.requester_did.as_deref() == Some(fixture.parent.did())
        });
        let response = child_access
            .execute(&format!(
                "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} relationships_pending }} }}",
                gents_store::graphql::escape_graphql_string(doc),
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let signed_by_parent =
            gents_store::identity::request_receipt::verify_request_receipt_signature(&row).is_ok()
                && row.admission_signer_did.as_deref() == Some(fixture.parent.did());
        let relationships_pending_after_watcher = response["data"]["AgentRequest"][0]
            ["relationships_pending"]
            .as_bool()
            .unwrap();
        let request = requests
            .into_iter()
            .find(|request| request.doc_id == doc)
            .expect("child sees its published opening");
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
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
        let claimed = matches!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );

        case.check(
            gents_lean_vocab::families::delegated_publication_recovery_watcher::RecoveryOutput {
                request_committed,
                tool_running: state == "running",
                child_read_before_watcher,
                relationships_pending_after_watcher,
                child_read_after_watcher,
                signed_by_parent,
                claimed,
            },
        );
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn existing_child_message_uses_calling_agent_native_claim() {
    for (active, deny) in [(false, false), (false, true), (true, true)] {
        let mut fixture = fixture(if active {
            "existing-active"
        } else if deny {
            "existing-idle-denied"
        } else {
            "existing-idle-allowed"
        })
        .await;
        let first = gents_session::session::message::commit(
            &fixture.node,
            &gents_automation::goal::session_message::SessionGoals,
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
            gents_store::graphql::escape_graphql_string(&first.request_doc_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        );
        let response = child_access.execute(&query).await.unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let opening = AgentRequest::try_from(row).unwrap();
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
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
        let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
            fixture.node.clone(),
            fixture.child.as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        child.begin_owned_execution(&writer).await.unwrap();
        if !active {
            child
                .terminalize_owned(
                    gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("fixture completed its first turn"),
                )
                .await
                .unwrap();
        }
        let mut tool =
            gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
                fixture.node.clone(),
                &mut fixture._owner,
                fixture.runtime.did(),
                1,
                gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
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
            let forged_plan = gents_session::session::message::plan_with_delivery(
                &fixture.node,
                &gents_automation::goal::session_message::SessionGoals,
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
            assert!(gents_session::session::message::commit(
                &fixture.node,
                &gents_automation::goal::session_message::SessionGoals,
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
        let plan = gents_session::session::message::plan_with_delivery(
            &fixture.node,
            &gents_automation::goal::session_message::SessionGoals,
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
        let receipt = gents_session::session::message::commit(
            &fixture.node,
            &gents_automation::goal::session_message::SessionGoals,
            &cause,
            &mut tool,
            plan,
            true,
        )
        .await
        .unwrap();
        let query = format!(
            "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            gents_store::graphql::escape_graphql_string(&receipt.request_doc_id),
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
        gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
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
            let blocked = gents_session::request::lifecycle::queue::steering_snapshot(
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
            let snapshot = gents_session::request::lifecycle::queue::steering_snapshot(
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
            gents_session::request::admission::sign_agent_request_create(
                fixture.parent.as_ref(),
                &mut forged,
            )
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
