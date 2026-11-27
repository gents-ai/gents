use gents_session::session::message::MessageContent;
use std::sync::Arc;

use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::SessionMessageCause;
use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_protocol::tool_call::AwaitMode;
use gents_session::request::admission::AgentRequestAdmissionVerifier;
use gents_session::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use gents_session::session::message::{AgentMessageDelivery, Plan, RenderedBody};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::identity::NodeIdentity;
use gents_store::test_support::STALL_CEILING;
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
use serde_json::json;

struct Fixture {
    _db: gents_store::test_support::TestStore,
    node: Arc<defra_node::Node>,
    access: ConfigAccess,
    config: ConfigAccess,
    parent_binding: gents_protocol::agent_identity::AgentIdentityBinding,
    child_binding: gents_protocol::agent_identity::AgentIdentityBinding,
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
    let db = crate::support::trial_store(&fixture_name).await;
    let (db, user, user_access) =
        gents_documents::test_support::SignedHome::enroll(db, &fixture_name)
            .await
            .into_parts();
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
        gents_protocol::request_admission::RequestScope::new(
            runtime.did(),
            user.did(),
            parent.agent_did.clone(),
            session_id,
        ),
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
            gents_toolcalls::call::lifecycle::admission_fixture::AcceptedFixtureCall {
                turn: 0,
                tool_name: gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
                tool_call_id: "finality-tool",
                arguments: json!({"agent":"child", "prompt":"work"}),
                await_mode: AwaitMode::Background,
                start_running: false,
            },
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
        agent_did: child.agent_did.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let signer = parent_identity.managed_agent_signer().unwrap();
    let mut plan = gents_session::session::message::plan_with_delivery(
        &node,
        &gents_automation::goal::session_message::SessionGoals,
        &cause,
        &target,
        MessageContent {
            rendered: RenderedBody {
                content: "work".into(),
                goal: None,
            },
            title: None,
        },
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
        child_binding: child,
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
                "the transaction must lose its post-commit acknowledgement: {result:?}"
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
            tokio::time::timeout(STALL_CEILING, async {
                tokio::select! {
                    _ = reached.notified() => {}
                    result = &mut child => panic!("child did not reach native commit boundary: {result:?}"),
                }
            }).await.expect("child reaches its native commit boundary");
            let (state, children) = observed(&fixture).await;
            assert_eq!(native(&state, &children, None), "pending");
            if failure_first {
                tokio::time::timeout(
                    STALL_CEILING,
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

async fn drop_commit_acknowledgement(fixture: &mut Fixture) {
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
    tokio::time::timeout(STALL_CEILING, async {
        tokio::select! {
            _ = reached.notified() => {}
            result = &mut dispatch => panic!("dispatch returned before cancellation: {result:?}"),
        }
    })
    .await
    .expect("child publication reaches the committed acknowledgement boundary");
    drop(dispatch);
}

async fn child_publication_row(fixture: &Fixture) -> serde_json::Value {
    let tool = gents_store::graphql::escape_graphql_string(&fixture.cause.tool_call_doc_id);
    let response = fixture
        .access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{tool}" }} }}) {{ _docID session_doc_id agent_doc_id relationships_pending }} }}"#,
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"][0].clone()
}

async fn recover_as_creator(fixture: &Fixture) {
    let watcher = gents_session::request::watcher::DefraWatcher::for_agents(
        fixture.node.clone(),
        fixture.runtime.did(),
        vec![fixture.parent.clone()],
    )
    .unwrap();
    let _ = watcher.pending_requests().await.unwrap();
    let _ = watcher.pending_requests().await.unwrap();
}

async fn child_claim(fixture: &Fixture, doc: &str) -> String {
    let watcher = gents_session::request::watcher::DefraWatcher::for_agents(
        fixture.node.clone(),
        fixture.runtime.did(),
        vec![fixture.child.clone()],
    )
    .unwrap();
    let Some(request) = watcher
        .pending_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.doc_id == doc)
    else {
        return "unavailable".into();
    };
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
    let outcome = lifecycle.claim_with_identity().await;
    match outcome {
        Ok(ClaimOutcome::Claimed) => "claimed".into(),
        Ok(ClaimOutcome::Queued) => "unavailable".into(),
        other => panic!("unmodeled child claim outcome {other:?}"),
    }
}

async fn child_agent_document(fixture: &Fixture) -> String {
    let agents = fixture
        .config
        .execute("{ Agent { _docID agent_did } }")
        .await
        .unwrap();
    agents["data"]["Agent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_did"] == fixture.child.did())
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn replace_child_agent_document(fixture: &Fixture) {
    let doc = child_agent_document(fixture).await;
    fixture
        .config
        .transact_write("test.finality.child_agent_removed", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.delete_document_with_schema("Agent", doc, gents_protocol::schemas::AGENT)
                    .await
            })
        })
        .await
        .unwrap();
    let docs = gents_documents::test_support::test_agent_documents(
        fixture.runtime.did(),
        "child",
        &fixture.child_binding,
    )
    .into_iter()
    .filter(|(collection, _)| matches!(collection, gents_store::collection::Collection::Agent))
    .map(|(collection, value)| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
    .collect();
    let plan = DesiredStateApplyPlan::new(docs).unwrap();
    fixture
        .config
        .transact_write("test.finality.child_agent_replaced", |txn| {
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
    assert_ne!(child_agent_document(fixture).await, doc);
}

#[tokio::test]
async fn delegated_publication_creator_recovery_follows_the_lean_model() {
    for case in gents_lean_vocab::families::delegated_publication_creator_recovery::cases() {
        let mut fixture = fixture(&case.input.name).await;
        let mut receipt_observed = false;
        for action in &case.input.actions {
            match action.kind.as_str() {
                "commit" => drop_commit_acknowledgement(&mut fixture).await,
                "observe" => {
                    receipt_observed = action.exact == Some(true) && action.owner == Some(true)
                }
                "publish" if action.actor.as_deref() == Some(case.input.parent_agent.as_str()) => {
                    if receipt_observed {
                        if action.exact != Some(true) {
                            replace_child_agent_document(&fixture).await;
                        }
                        recover_as_creator(&fixture).await;
                    }
                }
                "publish" => {
                    let row = child_publication_row(&fixture).await;
                    let _ =
                        gents_session::request::lifecycle::complete_client_request_relationships(
                            &fixture.config,
                            row["_docID"].as_str().unwrap(),
                        )
                        .await;
                }
                "revoke_session_parent" => {
                    let row = child_publication_row(&fixture).await;
                    let _ = fixture
                        .access
                        .delete_resource_relationship(
                            "AgentSession",
                            row["session_doc_id"].as_str().unwrap(),
                            "parent",
                            "agents",
                            &child_agent_document(&fixture).await,
                        )
                        .await;
                }
                other => panic!(
                    "unmodeled publication action {other} in {}",
                    case.input.name
                ),
            }
        }
        let (state, children) = observed(&fixture).await;
        let row = child_publication_row(&fixture).await;
        let doc = row["_docID"].as_str().unwrap();
        let session_query = format!(
            r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID relationships_pending }} }}"#,
            gents_store::graphql::escape_graphql_string(row["session_doc_id"].as_str().unwrap())
        );
        let session = fixture.access.execute(&session_query).await.unwrap();
        let owner_view = fixture.config.execute(&session_query).await.unwrap();
        let child_access =
            ConfigAccess::local_as(fixture.node.clone(), fixture.child.did().parse().unwrap());
        let child_view = child_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(doc)
            ))
            .await
            .unwrap();
        let claim = child_claim(&fixture, doc).await;
        case.check(
            gents_lean_vocab::families::delegated_publication_creator_recovery::CreatorRecoveryOutput {
                committed: children.len() == 1,
                tool_running: state == "running",
                session_pending: session["data"]["AgentSession"][0]["relationships_pending"]
                    .as_bool()
                    .unwrap(),
                request_pending: row["relationships_pending"].as_bool().unwrap(),
                target_read: !child_view["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                node_owner_read: !owner_view["data"]["AgentSession"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                claim,
            },
        );
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn delegated_publication_commit_refusal_follows_the_lean_model() {
    for case in gents_lean_vocab::families::delegated_publication_commit_refusal::cases() {
        let mut fixture = fixture(&case.input.name).await;
        let commit = case
            .input
            .actions
            .first()
            .unwrap_or_else(|| panic!("{} begins with its commit", case.input.name));
        let mut plan = fixture.plan.take().unwrap();
        let (gents_session::session::message::plan::PlannedWrite::Request { create, .. }
        | gents_session::session::message::plan::PlannedWrite::Goal { create, .. }) =
            &mut plan.write;
        if commit.starter == Some(false) {
            create.input.started_by.as_mut().unwrap().session_id = "unverified-starter".into();
        }
        if case.input.signer != case.input.parent_agent {
            create.requester_did = fixture.config.actor_did().unwrap();
        }
        if !case.input.invoke_grant {
            fixture
                .config
                .delete_actor_relationship(
                    "Agent",
                    &child_agent_document(&fixture).await,
                    "session_writer",
                    &fixture.parent_binding.agent_did.parse().unwrap(),
                )
                .await
                .unwrap();
        }
        let mut tool = fixture.tool.clone();
        let attempt = gents_session::session::message::commit(
            &fixture.node,
            &gents_automation::goal::session_message::SessionGoals,
            &fixture.cause,
            &mut tool,
            plan,
            false,
        );
        let _ = if commit.native_commit == Some(false) {
            ConfigApplyTxn::with_successful_mutation_failure_at(Some(1), attempt)
                .await
                .0
        } else {
            attempt.await
        };
        let (state, children) = observed(&fixture).await;
        let child_access =
            ConfigAccess::local_as(fixture.node.clone(), fixture.child.did().parse().unwrap());
        let child_view = child_access
            .execute("{ AgentRequest { _docID } }")
            .await
            .unwrap();
        let child_requests = child_view["data"]["AgentRequest"].as_array().unwrap();
        let claim = match children.first().and_then(|child| child["_docID"].as_str()) {
            Some(doc) => child_claim(&fixture, doc).await,
            None => "unavailable".into(),
        };
        case.check(
            gents_lean_vocab::families::delegated_publication_commit_refusal::CommitRefusalOutput {
                committed: children.len() == 1,
                tool_running: state == "running",
                target_read: !child_requests.is_empty(),
                claim,
            },
        );
        fixture.node.shutdown().await.unwrap();
    }
}

/// Writes the modeled concurrent change while the child's claim is held
/// between its reads and its native commit.
async fn land_claim_race(fixture: &Fixture, doc: &str, race: &str) {
    match race {
        "revoke_invoke" => fixture
            .config
            .delete_actor_relationship(
                "Agent",
                &child_agent_document(fixture).await,
                "session_writer",
                &fixture.parent_binding.agent_did.parse().unwrap(),
            )
            .await
            .unwrap(),
        "rewrite_markers" | "reset_request_marker" => {
            let row = child_publication_row(fixture).await;
            let request_pending = race == "reset_request_marker";
            let session = row["session_doc_id"].as_str().unwrap().to_owned();
            fixture
                .access
                .transact_write("test.finality.claim_race_markers", |txn| {
                    let session = &session;
                    Box::pin(async move {
                        for (collection, schema, doc, pending) in [
                            (
                                "AgentSession",
                                gents_protocol::schemas::AGENT_SESSION,
                                session.as_str(),
                                false,
                            ),
                            (
                                "AgentRequest",
                                gents_protocol::schemas::AGENT_REQUEST,
                                doc,
                                request_pending,
                            ),
                        ] {
                            txn.execute_client_signed_update(
                                &format!(
                                    r#"mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: "{}", input: $input) {{ _docID }} }}"#,
                                    gents_store::graphql::escape_graphql_string(doc)
                                ),
                                &json!({"input": {"relationships_pending": pending}}),
                                schema,
                            )
                            .await?;
                        }
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
        }
        other => panic!("unmodeled claim race {other}"),
    }
}

/// The child's claim with each modeled race landing on one attempt. The
/// fixture's commit pause holds only the first attempt, so a case races at
/// most once; a rerun reads and decides again on the state the race left.
async fn child_claim_raced(fixture: &Fixture, doc: &str, races: &[String]) -> String {
    let [race] = races else {
        assert!(races.is_empty(), "the fixture races one attempt: {races:?}");
        return child_claim(fixture, doc).await;
    };
    let watcher = gents_session::request::watcher::DefraWatcher::for_agents(
        fixture.node.clone(),
        fixture.runtime.did(),
        vec![fixture.child.clone()],
    )
    .unwrap();
    let request = watcher
        .pending_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.doc_id == doc)
        .expect("the published child request is pending for the child");
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
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut claim = Box::pin(ConfigApplyTxn::with_embedded_before_commit_pause(
        reached.clone(),
        release.clone(),
        lifecycle.claim_with_identity(),
    ));
    tokio::time::timeout(STALL_CEILING, async {
        tokio::select! {
            _ = reached.notified() => {}
            outcome = &mut claim => panic!("claim returned before its commit boundary: {outcome:?}"),
        }
    })
    .await
    .expect("the claim reaches its native commit boundary");
    land_claim_race(fixture, doc, race).await;
    release.notify_one();
    match claim.await {
        Ok(ClaimOutcome::Claimed) => "claimed".into(),
        Ok(ClaimOutcome::Queued) => "unavailable".into(),
        Ok(other) => panic!("unmodeled raced claim outcome {other:?}"),
        Err(error) => {
            let row = fixture
                .config
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    gents_store::graphql::escape_graphql_string(doc)
                ))
                .await
                .unwrap();
            assert_eq!(
                row["data"]["AgentRequest"][0]["lifecycle_state"],
                gents_protocol::request_lifecycle::RequestLifecycleState::Failed.as_str(),
                "a refused claim rejects the request: {error:#}"
            );
            "denied".into()
        }
    }
}

#[tokio::test]
async fn delegated_publication_claim_race_follows_the_lean_model() {
    for case in gents_lean_vocab::families::delegated_publication_claim_race::cases() {
        let mut fixture = fixture(&case.input.name).await;
        assert!(
            case.input
                .actions
                .first()
                .is_some_and(|action| action.kind == "commit"
                    && action.starter == Some(true)
                    && action.creation_receipt == Some(true)
                    && action.native_commit == Some(true)),
            "{} publishes through one complete commit",
            case.input.name
        );
        assert!(
            case.input.invoke_grant,
            "the fixture's configuring User grants the parent session_writer"
        );
        let mut tool = fixture.tool.clone();
        gents_session::session::message::commit(
            &fixture.node,
            &gents_automation::goal::session_message::SessionGoals,
            &fixture.cause,
            &mut tool,
            fixture.plan.take().unwrap(),
            false,
        )
        .await
        .unwrap();
        let (_, children) = observed(&fixture).await;
        let doc = children[0]["_docID"].as_str().unwrap().to_owned();
        let claim = child_claim_raced(&fixture, &doc, &case.input.claim_races).await;
        case.check(claim);
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn dropped_commit_ack_recovery_matches_the_lean_model() {
    for case in gents_lean_vocab::families::delegated_publication_recovery_watcher::cases() {
        let mut fixture = fixture(&case.input.name).await;
        drop_commit_acknowledgement(&mut fixture).await;
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
