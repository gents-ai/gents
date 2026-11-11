use super::target::session_target_allowed;
use super::*;
use crate::tools::call::lifecycle::admission_fixture::{
    published_session_message, PublishedAdmissionOptions, PublishedSessionMessage,
};
use gents_protocol::tool_call::AwaitMode;
use gents_store::identity::NodeIdentity as _;

pub(super) async fn started(
    name: &str,
) -> (
    PublishedSessionMessage,
    gents_protocol::row::AgentRequest,
    gents_protocol::row::AgentRequest,
) {
    let message = published_session_message(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let caller = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        message.admission.tool.request_doc_id().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let caused = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    materialize_session(&message.admission.tool.access().unwrap(), &caused).await;
    (message, caller, caused)
}

/// The calling Agent's signed access. Its session-message rows are Agent-owned, so
/// ACP hides them from the node identity.
fn owner(message: &PublishedSessionMessage) -> ConfigAccess {
    message.admission.tool.access().unwrap()
}

/// The calling Agent's DID, which signs the session-message interrupt checks.
fn caller_agent_did(message: &PublishedSessionMessage) -> String {
    message.admission.tool.actor().to_string()
}

/// `commit` already materializes the caused session; this is the idempotent
/// re-check, which must read the session through its owning Agent.
async fn materialize_session(access: &ConfigAccess, caused: &gents_protocol::row::AgentRequest) {
    // Sessions record created_at in the canonical second-precision UTC form.
    materialize_session_at(
        access,
        caused,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
    .await;
}

async fn materialize_session_at(
    access: &ConfigAccess,
    caused: &gents_protocol::row::AgentRequest,
    created_at: &str,
) {
    let caused = caused.clone();
    let created_at = created_at.to_owned();
    access
        .transact_write("test.materialize_session", move |txn| {
            let caused = caused.clone();
            let created_at = created_at.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &caused.session_id,
                    &caused.node_did,
                    &caused.agent_did,
                    caused.requester_did.as_deref(),
                    None,
                    crate::request::lifecycle::materialize::created_session_provenance(&caused),
                    &created_at,
                )
                .await
            })
        })
        .await
        .unwrap();
}

async fn set_state(access: &ConfigAccess, doc_id: &str, state: &str) {
    access
        .write(
            "test.set_state",
            &format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
                escape_graphql_string(doc_id)
            ),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn steer_reachability_matches_lean() {
    use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
    let (message, _, head) = started("steer-reachability-model").await;
    let cases = &gents_lean_vocab::lean_contract_snapshot().agent_steer_reachability_cases;
    for case in cases {
        let rows = case
            .inputs
            .iter()
            .map(|item| {
                let (delivery, source, agent) = match item.kind.as_str() {
                    "queued" => (QueueDelivery::Queue, QueueSource::User, false),
                    "user_steer" => (QueueDelivery::Steer, QueueSource::User, false),
                    "agent_steer" => (QueueDelivery::Steer, QueueSource::Steering, true),
                    "blocked" => (
                        QueueDelivery::Queue,
                        QueueSource::BackgroundCompletion,
                        false,
                    ),
                    other => panic!("unexpected modeled boundary kind: {other}"),
                };
                let mut input = head.input.clone();
                input.cwd = if item.compatible {
                    head.input.cwd.clone()
                } else {
                    Some("/incompatible".into())
                };
                input.queue = Some(RequestQueue {
                    delivery,
                    source,
                    policy: QueuePolicy::Append,
                    position: None,
                    key: None,
                    queued_after_request_id: agent.then(|| head.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                });
                gents_protocol::row::AgentRequestRow {
                    doc_id: Some(item.id.to_string()),
                    request_id: item.id.to_string(),
                    node_did: Some(head.node_did.clone()),
                    session_id: Some(head.session_id.clone()),
                    requester_did: head.requester_did.clone(),
                    agent_did: Some(head.agent_did.clone()),
                    purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                    execution_origin: Some("interactive".into()),
                    caused_by_parent_request_doc_id: agent.then(|| "parent".into()),
                    caused_by_parent_tool_call_doc_id: agent.then(|| "tool".into()),
                    input: Some(input),
                    ..Default::default()
                }
            })
            .collect();
        let actual = crate::request::lifecycle::queue::steer_reachable(
            &head,
            rows,
            &case.target.to_string(),
            chrono::Utc::now(),
        )
        .is_ok();
        assert_eq!(actual, case.expected, "{case:?}");
    }
    message
        .admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn steer_reaches_a_head_parked_for_its_backend() {
    let (message, caller, caused) = started("agent-steer-parked-head").await;
    let node = message.admission.node.clone();
    set_state(&owner(&message), &caused.doc_id, "waitingForBackend").await;
    let cause = super::hop_tests::cause(&message, caller.request_hop).await;
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    let plan = plan_with_delivery(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        RenderedBody {
            content: "change course".to_owned(),
            goal: None,
        },
        None,
        AgentMessageDelivery::Steer,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    let (cause_ref, plan_ref) = (&cause, &plan);
    let receipt = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("test.steer.parked_head", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, cause_ref, plan_ref).await
            })
        })
        .await
        .expect("a steer planned against a parked head is published");
    let row = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &receipt.doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.input.queue.unwrap().delivery,
        gents_protocol::request_input::QueueDelivery::Steer
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn explicit_steer_checks_the_physical_head_at_publication() {
    let (message, caller, caused) = started("agent-steer-head-change").await;
    let node = message.admission.node.clone();
    set_state(&owner(&message), &caused.doc_id, "processing").await;
    // Staging verifies the source edge, so the cause must name the accepted tool row.
    let cause = super::hop_tests::cause(&message, caller.request_hop).await;
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    let make_plan = || async {
        plan_with_delivery(
            &node,
            &NoSessionMessageGoals,
            &cause,
            &target,
            RenderedBody {
                content: "change course".to_owned(),
                goal: None,
            },
            None,
            AgentMessageDelivery::Steer,
            &super::hop_tests::signer(&message),
        )
        .await
        .unwrap()
        .unwrap()
    };
    let stale = make_plan().await;
    set_state(&owner(&message), &caused.doc_id, "completed").await;
    let (cause_ref, stale_ref) = (&cause, &stale);
    let result = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("test.steer.changed_head", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, cause_ref, stale_ref).await
            })
        })
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed while the steer was planned"));
    set_state(&owner(&message), &caused.doc_id, "processing").await;
    let queued = plan_with_delivery(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        RenderedBody {
            content: "queued turn".to_owned(),
            goal: None,
        },
        None,
        AgentMessageDelivery::Queue,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    let (cause_ref, queued_ref) = (&cause, &queued);
    let queued_receipt = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("test.steer.queued_predecessor", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, cause_ref, queued_ref).await
            })
        })
        .await
        .unwrap();
    let current = make_plan().await;
    let (cause_ref, current_ref) = (&cause, &current);
    let receipt = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("test.steer.compatible", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, cause_ref, current_ref)
                    .await
            })
        })
        .await
        .expect("compatible steer is published");
    let row = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &receipt.doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.input.queue.unwrap().delivery,
        gents_protocol::request_input::QueueDelivery::Steer
    );
    let (node_did, session_id) = (&caused.node_did, &caused.session_id);
    let snapshot = owner(&message)
        .transact_readonly("test.steer.predecessor_order", move |txn| {
            Box::pin(async move {
                crate::request::lifecycle::queue::pending_rows_in_txn(txn, node_did, session_id)
                    .await
            })
        })
        .await
        .unwrap();
    assert!(snapshot
        .iter()
        .any(|row| row.doc_id.as_deref() == Some(queued_receipt.doc_id.as_str())));
    let candidate = snapshot
        .iter()
        .find(|row| row.doc_id.as_deref() == Some(receipt.doc_id.as_str()))
        .unwrap();
    let mut contexts = Vec::new();
    let mut human = caused.clone();
    human.requester_did = Some("did:test:human".to_owned());
    contexts.push(("human requester", human));
    let mut cwd = caused.clone();
    cwd.input.cwd = Some("/different-cwd".to_owned());
    contexts.push(("cwd", cwd));
    let mut skills = caused.clone();
    skills
        .input
        .selected_skill_ids
        .push("different-skill".to_owned());
    contexts.push(("skills", skills));
    let mut workspace = caused.clone();
    workspace.workspace_id = Some("different-workspace".to_owned());
    contexts.push(("workspace", workspace));
    let mut goal = caused.clone();
    goal.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
        goal_id: "different-goal".to_owned(),
        root_request_doc_id: None,
    });
    contexts.push(("goal", goal));
    let modeled = gents_lean_vocab::lean_contract_snapshot()
        .agent_steer_disposition_cases
        .iter()
        .find(|case| case.active && !case.compatible)
        .unwrap();
    for case in &gents_lean_vocab::lean_contract_snapshot().agent_steer_target_cases {
        let mut head = caused.clone();
        if case["planned_head"] != case["active_head"] {
            head.request_id = "a-later-turn".to_owned();
        }
        assert_eq!(
            crate::request::lifecycle::queue::steering_compatible(
                &head,
                candidate,
                chrono::Utc::now()
            ),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    for (name, head) in contexts {
        let disposition = if crate::request::lifecycle::queue::steering_compatible(
            &head,
            candidate,
            chrono::Utc::now(),
        ) {
            "steer"
        } else {
            "refuse"
        };
        assert_eq!(disposition, modeled.expected, "{name}");
    }
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn only_the_starting_session_may_interrupt() {
    let (message, caller, caused) = started("agent-interrupt-permission").await;
    let node = message.admission.node.clone();
    let caller = SessionScope::of_request(&caller);
    let target = SessionScope::of_request(&caused);
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &caller, &target)
            .await
            .unwrap()
            .is_none()
    );
    let another = SessionScope {
        session_id: "another-session".to_owned(),
        ..caller.clone()
    };
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &another, &target)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &target, &caller)
            .await
            .unwrap()
            .is_some()
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn signed_creation_waits_for_its_canonical_parent_evidence() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "delayed-agent-creation-evidence".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caller = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        message.admission.tool.request_doc_id().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let caused = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let access = owner(&message);
    let call_doc = message.admission.tool.doc_id().unwrap();
    let set_name = |name: &str| {
        format!(
        "mutation {{update_AgentToolCall(docID: \"{}\", input: {{tool_name: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(call_doc), escape_graphql_string(name),
    )
    };
    access
        .write("test.hide_creation_cause", &set_name("agent_message"))
        .await
        .unwrap();
    materialize_session(&owner(&message), &caused).await;
    let target = SessionScope::of_request(&caused);
    let source = SessionScope::of_request(&caller);
    let session = crate::session::origin::load_session(
        &access,
        &target.session_id,
        target.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(session.provenance.unwrap().creation_receipt.is_some());
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &source, &target)
            .await
            .unwrap()
            .is_some()
    );
    access
        .write(
            "test.restore_creation_cause",
            &set_name(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME),
        )
        .await
        .unwrap();
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &source, &target)
            .await
            .unwrap()
            .is_none()
    );
    let mut forged = caused.input.clone();
    forged.session_creation = Some(gents_protocol::request_input::SessionCreationIntent::PluginJob);
    let mutation = format!("mutation($input: JSON) {{update_AgentRequest(docID: \"{}\", input: {{input: $input}}) {{_docID}}}}", escape_graphql_string(&caused.doc_id));
    let rejected = access
        .transact_write("test.forge_creation_intent", |txn| {
            let mutation = &mutation;
            let forged = &forged;
            Box::pin(async move {
                txn.execute_with_variables(mutation, &serde_json::json!({"input": forged}))
                    .await
            })
        })
        .await
        .unwrap_err();
    assert!(
        rejected.to_string().contains("immutable field"),
        "{rejected:#}"
    );
    assert!(
        interrupt_refusal(&node, &caller_agent_did(&message), &source, &target)
            .await
            .unwrap()
            .is_none()
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn remote_signed_creation_survives_parent_evidence_arriving_after_claim() {
    use crate::tools::call::lifecycle::admission_fixture::{
        materialize_session_message, published_admission_with_owner,
    };

    // P2P carries only discovery and enrollment, so a session message to another
    // runtime is refused at planning and no request is replicated to the target.
    let (mut source, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "remote-delayed-creation".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.into()),
        start_running: false,
        ..Default::default()
    })
    .await
    .unwrap();
    let error = materialize_session_message(
        &source.node,
        &owner,
        &mut source.tool,
        "did:key:remote-runtime",
        "remote work",
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cross-runtime agent delivery requires the authenticated HTTP delivery path"),
        "{error:#}"
    );
    source.node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&source.path).unwrap();
}

#[tokio::test]
async fn forged_descriptive_provenance_and_altered_creation_receipt_cannot_grant_control() {
    let (message, caller, caused) = started("agent-interrupt-receipt").await;
    let node = message.admission.node.clone();
    let caller_scope = SessionScope::of_request(&caller);
    let target_scope = SessionScope::of_request(&caused);
    let access = owner(&message);
    let session = crate::session::origin::load_session(
        &access,
        &target_scope.session_id,
        target_scope.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    let mut forged = session.provenance.unwrap();
    assert!(
        forged.creation_receipt.is_some(),
        "newly materialized session has a receipt"
    );
    forged.creation_receipt.as_mut().unwrap().signature[0] ^= 1;
    let mutation = format!(
        r#"mutation($provenance: JSON) {{update_AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, input: {{provenance: $provenance}}) {{_docID}}}}"#,
        escape_graphql_string(&target_scope.node_did),
        escape_graphql_string(&target_scope.session_id),
    );
    access
        .transact_write("test.corrupt_session_receipt", |txn| {
            let mutation = &mutation;
            let forged = &forged;
            Box::pin(async move {
                txn.execute_with_variables(mutation, &serde_json::json!({"provenance": forged}))
                    .await
            })
        })
        .await
        .unwrap();
    assert!(interrupt_refusal(
        &node,
        &caller_agent_did(&message),
        &caller_scope,
        &target_scope
    )
    .await
    .unwrap()
    .is_some());

    let human = "unrelated-human-session";
    let provenance = gents_protocol::session::SessionProvenance {
        parent_request_doc_id: Some(caller.doc_id.clone()),
        opening_request_doc_id: Some(caused.doc_id.clone()),
        ..Default::default()
    };
    let created_at = chrono::Utc::now().to_rfc3339();
    access.transact_write("test.forged_session_provenance", |txn| {
        let provenance = &provenance;
        let caller = &caller;
        let created_at = &created_at;
        Box::pin(async move {
            txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                &serde_json::json!({"input": {"node_did": caller.node_did, "session_id": human,
                    "requester_did": caller.node_did, "agent_did": caller.agent_did,
                    "created_at": created_at, "provenance": provenance}}),
            ).await
        })
    }).await.unwrap();
    let human_scope = SessionScope {
        session_id: human.into(),
        ..caller_scope.clone()
    };
    assert!(interrupt_refusal(
        &node,
        &caller_agent_did(&message),
        &caller_scope,
        &human_scope
    )
    .await
    .unwrap()
    .is_some());
    let lineage = crate::session::origin::lineage(&access, &caller_scope)
        .await
        .unwrap();
    for scope in [&human_scope, &target_scope] {
        assert!(
            !lineage.started.iter().any(|link| &link.scope == scope),
            "{scope:?}: a parent pointer or an altered receipt is not a verified starter"
        );
    }
    assert_eq!(
        crate::session::origin::started_by(&access, &human_scope)
            .await
            .unwrap(),
        None
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn interrupt_stops_the_turn_and_the_steer_is_a_new_request() {
    let (message, caller, caused) = started("agent-interrupt-steer").await;
    let node = message.admission.node.clone();
    let scope = SessionScope::of_request(&caused);
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    assert_eq!(
        interrupt_session_with_access(&node, &owner(&message), &scope)
            .await
            .unwrap(),
        None
    );
    set_state(
        &owner(&message),
        &message.caused_request_doc_id,
        "processing",
    )
    .await;

    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: "caller-session".to_owned(),
        caller_requester_did: None,
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: "steer-tool".to_owned(),
        tool_call_doc_id: "steer-tool-doc".to_owned(),
        correlation: None,
    };
    let body = || RenderedBody {
        content: "change course".to_owned(),
        goal: None,
    };
    let steering = plan(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        body(),
        None,
        false,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(steering.delivery(), Delivery::Queued);
    let steer = plan(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        body(),
        None,
        true,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(steer.delivery(), Delivery::Request);
    assert_eq!(steer.hop(), 1);

    assert_eq!(
        interrupt_session_with_access(&node, &owner(&message), &scope)
            .await
            .unwrap(),
        Some(caused.request_id.clone())
    );
    assert!(
        crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id_with_access(
            &owner(&message),
            &message.caused_request_doc_id
        )
        .await
        .unwrap()
        .is_some()
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn explicit_stop_drains_pending_work_and_rejects_late_admission() {
    let (message, caller, caused) = started("agent-stop-drains").await;
    let node = message.admission.node.clone();
    let receipt = crate::session::stop_session_with_access(
        &node,
        &owner(&message),
        &caused.node_did,
        &caused.session_id,
        caused.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "stop_requested");
    assert_eq!(receipt.drained_request_ids, vec![caused.request_id.clone()]);
    assert!(crate::session::session_is_stopped(
        &node,
        &identity::Did::new(caused.agent_did.clone()).unwrap(),
        &caused.node_did,
        &caused.session_id,
        caused.requester_did.as_deref(),
    )
    .await
    .unwrap());

    let error = plan(
        &node,
        &NoSessionMessageGoals,
        &SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_did: caller.agent_did.clone(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop: caller.request_hop,
            tool_call_id: "late-message".to_owned(),
            tool_call_doc_id: "late-message-doc".to_owned(),
            correlation: None,
        },
        &SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_did: caused.agent_did.clone(),
            session_id: caused.session_id.clone(),
        },
        RenderedBody {
            content: "too late".to_owned(),
            goal: None,
        },
        None,
        false,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap();
    assert!(
        error.is_err(),
        "late message admission must honor durable stop"
    );

    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn agent_lists_separate_targets_from_messageable_sessions() {
    let (message, caller, caused) = started("agent-list").await;
    let node = message.admission.node.clone();
    let tools = AgentToolConfig {
        enabled: true,
        targets: vec![gents_documents::AgentTargetDocument {
            target_id: "worker".to_owned(),
            node_did: caller.node_did.clone(),
            target_node_did: caused.node_did.clone(),
            agent_did: caused.agent_did.clone(),
            name: "worker".to_owned(),
            description: Some("does the work".to_owned()),
            tags: Vec::new(),
        }],
    };
    let listed = agent_list(&node, &caller, &tools).await.unwrap();
    assert_eq!(listed["agents"][0]["agent"], "worker");
    assert_eq!(listed["agents"][0]["agent_did"], caused.agent_did);
    assert!(listed.get("sessions").is_none());
    let listed = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{listed}");
    assert_eq!(sessions[0]["session_id"], caused.session_id);
    assert_eq!(sessions[0]["relationship"], "started_by_you");
    assert_eq!(sessions[0]["can_message"], true);
    assert_eq!(sessions[0]["can_interrupt"], true);
    assert_eq!(sessions[0]["status"], "idle");

    let from_child = agent_session_list(&node, &caused, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let sessions = from_child["sessions"].as_array().unwrap();
    assert!(
        sessions
            .iter()
            .any(|session| session["session_id"] == caller.session_id
                && session["relationship"] == "started_you"),
        "{from_child}"
    );
    for session in sessions {
        assert_eq!(
            resolve_send_target(
                &node,
                &caused.node_did,
                &caused.agent_did,
                &tools,
                session["session_id"].as_str().unwrap()
            )
            .await
            .unwrap()
            .is_some(),
            session["can_message"] == true,
            "{session}"
        );
    }
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

/// The caller-visible session and grants one Lean session-target case describes,
/// as `caller` on node `caller`.
fn session_target_fixture(
    same_requester: bool,
    same_node: bool,
    caused: bool,
    allowed_target: bool,
) -> (gents_protocol::session::AgentSession, AgentToolConfig) {
    use gents_protocol::session::{AgentSession, SessionProvenance};
    let session = AgentSession {
        session_id: "worker-session".to_owned(),
        node_did: if same_node { "caller" } else { "peer" }.to_owned(),
        requester_did: Some(if same_requester { "caller" } else { "other" }.to_owned()),
        agent_did: "worker".to_owned(),
        created_at: "2026-10-14T00:00:00Z".to_owned(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        observation: None,
        provenance: caused.then(|| SessionProvenance {
            parent_request_doc_id: Some("parent".to_owned()),
            ..Default::default()
        }),
    };
    let tools = AgentToolConfig {
        enabled: true,
        targets: if allowed_target {
            vec![gents_documents::AgentTargetDocument {
                target_id: "worker".to_owned(),
                node_did: "caller".to_owned(),
                target_node_did: session.node_did.clone(),
                agent_did: "worker".to_owned(),
                name: "worker".to_owned(),
                description: None,
                tags: Vec::new(),
            }]
        } else {
            Vec::new()
        },
    };
    (session, tools)
}

#[test]
fn session_target_authority_matches_lean_owner() {
    let cases = &gents_lean_vocab::lean_causal_hop_contract().session_target_cases;
    assert_eq!(cases.len(), 16);
    for case in cases {
        let (session, tools) = session_target_fixture(
            case.same_requester,
            case.same_node,
            case.caused,
            case.allowed_target,
        );
        assert_eq!(
            session_target_allowed(&session, "caller", "caller", &tools),
            case.expected_allowed,
            "{case:?}"
        );
    }
}

#[test]
fn session_list_entries_match_lean_owner() {
    let cases = &gents_lean_vocab::lean_causal_hop_contract().session_list_cases;
    assert_eq!(cases.len(), 64);
    for case in cases {
        let (session, tools) = session_target_fixture(
            case.same_requester,
            case.same_node,
            case.caused,
            case.allowed_target,
        );
        let entry = super::discovery::listed_session(
            session_target_allowed(&session, "caller", "caller", &tools),
            case.verified_starter_receipt,
            case.interrupt_allowed,
        );
        assert_eq!(entry.is_some(), case.expected_listed, "{case:?}");
        assert_eq!(
            entry.is_some_and(|entry| entry.can_message),
            case.expected_can_message,
            "{case:?}"
        );
        assert_eq!(
            entry.is_some_and(|entry| entry.can_interrupt),
            case.expected_can_interrupt,
            "{case:?}"
        );
    }
}

/// A User starts Agent A; A starts Agent B through `agent_new`. B lists A's
/// session, which the User requested, from B's own verified creation receipt;
/// A lists B's; an unrelated Agent C lists neither.
#[tokio::test]
async fn session_list_names_the_starter_from_the_callers_own_receipt() {
    use crate::tools::call::lifecycle::admission_fixture::published_admission_with_owner;

    let (mut admission, request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "session-list-starter".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        start_running: false,
        tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let caller = request.request().clone();
    assert_ne!(
        caller.requester_did.as_deref(),
        Some(caller.agent_did.as_str()),
        "the root session is requested by the User, not Agent A"
    );
    let caller_access = admission.tool.access().unwrap();
    let signer = caller_access.managed_agent_signer().unwrap();
    let root =
        gents_store::identity::KeyIdentity::load_existing(admission.path.join("node.key"), None)
            .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let worker = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        &caller.node_did,
        "started worker",
        &binding,
        &admission.client,
    )
    .await;
    let worker_rows = admission
        .client
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(&binding.agent_did)
        ))
        .await
        .unwrap();
    let worker_doc = worker_rows["data"]["Agent"][0]["_docID"]
        .as_str()
        .expect("installed worker Agent")
        .to_owned();
    for (relation, actor) in [
        ("session_writer", caller.agent_did.as_str()),
        ("executor", worker.did()),
    ] {
        admission
            .client
            .add_actor_relationship(
                "Agent",
                &worker_doc,
                relation,
                &identity::Did::new(actor).unwrap(),
            )
            .await
            .unwrap();
    }
    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: admission.tool.tool_call_id().to_owned(),
        tool_call_doc_id: admission.tool.doc_id().unwrap().to_owned(),
        correlation: None,
    };
    let target = SessionMessageTarget {
        node_did: caller.node_did.clone(),
        agent_did: worker.did().to_owned(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut started_plan = plan(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        RenderedBody {
            content: "start the worker".into(),
            goal: None,
        },
        None,
        false,
        &signer,
    )
    .await
    .unwrap()
    .unwrap();
    started_plan
        .sign_agent_creation(&cause.caller_node_did, &caller.input, &signer)
        .await
        .unwrap();
    let receipt = commit(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &mut admission.tool,
        started_plan,
        false,
    )
    .await
    .unwrap();
    let started = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &caller_access,
        &receipt.request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(started.agent_did, worker.did());
    assert_eq!(
        started.requester_did.as_deref(),
        Some(caller.agent_did.as_str())
    );
    let no_targets = AgentToolConfig {
        enabled: true,
        targets: Vec::new(),
    };

    let from_worker = agent_session_list(
        &node,
        &started,
        &no_targets,
        &AgentSessionListArgs::default(),
    )
    .await
    .unwrap();
    let sessions = from_worker["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{from_worker}");
    assert_eq!(sessions[0]["session_id"], caller.session_id);
    assert_eq!(sessions[0]["node_did"], caller.node_did);
    assert_eq!(sessions[0]["agent_did"], caller.agent_did);
    let worker_session = crate::session::origin::load_session(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(&started.agent_did).unwrap(),
        ),
        &started.session_id,
        started.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .expect("worker session");
    let creation = worker_session
        .provenance
        .and_then(|provenance| provenance.creation_receipt)
        .expect("worker creation receipt");
    assert!(
        gents_store::identity::verify_did_signature(
            sessions[0]["agent_did"].as_str().unwrap(),
            &creation.signing_payload(),
            &creation.signature,
        )
        .unwrap(),
        "the starter entry's agent_did is the receipt signer"
    );
    assert_eq!(sessions[0]["relationship"], "started_you");
    assert_eq!(sessions[0]["can_message"], false);
    assert_eq!(sessions[0]["can_interrupt"], false);
    assert!(
        sessions[0].get("title").is_none() && sessions[0].get("status").is_none(),
        "the starter entry reads none of the starter's documents: {from_worker}"
    );
    let (node_ref, started_ref, tools_ref) = (&node, &started, &no_targets);
    let filtered = move |filter: serde_json::Value| {
        let (node, started, tools) = (node_ref, started_ref, tools_ref);
        async move {
            agent_session_list(
                node,
                started,
                tools,
                &serde_json::from_value(serde_json::json!({ "filter": filter })).unwrap(),
            )
            .await
            .unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    assert_eq!(
        filtered(serde_json::json!({"relationship": "started_you"})).await,
        1
    );
    assert_eq!(
        filtered(serde_json::json!({"relationship": "same_node"})).await,
        0
    );
    assert_eq!(
        filtered(serde_json::json!({"agent": caller.agent_did})).await,
        1
    );
    assert_eq!(filtered(serde_json::json!({"status": "busy"})).await, 0);

    let from_starter = agent_session_list(
        &node,
        &caller,
        &no_targets,
        &AgentSessionListArgs::default(),
    )
    .await
    .unwrap();
    let sessions = from_starter["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{from_starter}");
    assert_eq!(sessions[0]["session_id"], started.session_id);
    assert_eq!(sessions[0]["relationship"], "started_by_you");
    assert_eq!(sessions[0]["can_message"], true);
    assert_eq!(sessions[0]["can_interrupt"], true);

    let unrelated = crate::tools::call::lifecycle::admission_fixture::claimed_signed_request(
        &node,
        "request-session-list-unrelated",
        "session-session-list-unrelated",
        &root,
        None,
    )
    .await;
    let unrelated = unrelated.request().clone();
    assert_ne!(unrelated.agent_did, caller.agent_did);
    assert_ne!(unrelated.agent_did, started.agent_did);
    let from_unrelated = agent_session_list(
        &node,
        &unrelated,
        &no_targets,
        &AgentSessionListArgs::default(),
    )
    .await
    .unwrap();
    assert_eq!(from_unrelated["sessions"], serde_json::json!([]));

    node.shutdown().await.unwrap();
    std::fs::remove_dir_all(&admission.path).unwrap();
}

#[tokio::test]
async fn session_listing_pages_by_logical_labels_and_compares_timestamp_instants() {
    let (message, caller, caused) = started("session-list-order-and-time").await;
    let node = message.admission.node.clone();
    for label in ["a-worker", "z-worker", "m-worker"] {
        let mut candidate = caused.clone();
        candidate.session_id = label.to_owned();
        candidate.caused_by_parent_request_doc_id = None;
        materialize_session_at(&owner(&message), &candidate, "2026-10-13T00:00:00Z").await;
    }
    let tools = AgentToolConfig {
        enabled: true,
        targets: Vec::new(),
    };
    let mut args: AgentSessionListArgs = serde_json::from_value(serde_json::json!({
        "limit": 1, "filter": {"relationship": "same_node"}
    }))
    .unwrap();
    let mut ordered = Vec::new();
    loop {
        let page = agent_session_list(&node, &caller, &tools, &args)
            .await
            .unwrap();
        ordered.extend(
            page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap().to_owned()),
        );
        args.cursor = page["next_cursor"].as_str().map(str::to_owned);
        if args.cursor.is_none() {
            break;
        }
    }
    assert_eq!(ordered, ["z-worker", "m-worker", "a-worker"]);
    for field in ["created_after", "updated_after"] {
        for (time, included) in [
            ("2026-10-12T23:59:59Z", true),
            ("2026-10-13T00:59:59+01:00", true),
            ("2026-10-13T00:00:00Z", false),
            ("2026-10-13T00:00:00+00:00", false),
            ("2026-10-13T00:00:00.001Z", false),
            ("2026-10-12T19:00:01-05:00", false),
        ] {
            let mut filter = serde_json::json!({"relationship": "same_node"});
            filter[field] = serde_json::json!(time);
            let args = serde_json::from_value(serde_json::json!({"filter": filter})).unwrap();
            let page = agent_session_list(&node, &caller, &tools, &args)
                .await
                .unwrap();
            let ids = page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                if included {
                    vec!["z-worker", "m-worker", "a-worker"]
                } else {
                    Vec::new()
                },
                "{field}: {time}"
            );
        }
    }
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn session_listing_discovers_unlinked_work_and_matches_send_authority() {
    let (message, mut caller_owner) =
        crate::tools::call::lifecycle::admission_fixture::published_session_message_with_owner(
            PublishedAdmissionOptions {
                name: "agent-session-discovery".to_owned(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let caller = caller_owner.request().clone();
    let node = message.admission.node.clone();
    let caused = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    materialize_session(&owner(&message), &caused).await;
    let tools = AgentToolConfig {
        enabled: true,
        targets: Vec::new(),
    };
    let mut candidates = Vec::new();
    for (name, owner, requester, parent) in [
        // A same-node session with no parent link is messageable only when its
        // requester is the calling Agent, which is how trigger runs record it.
        (
            "trigger-worker",
            caller.node_did.as_str(),
            caller.agent_did.as_str(),
            None,
        ),
        ("other-requester", caller.node_did.as_str(), "other", None),
        ("remote-root", "peer", caller.node_did.as_str(), None),
        (
            "remote-started",
            "peer",
            caller.node_did.as_str(),
            Some(caller.doc_id.clone()),
        ),
    ] {
        let mut candidate = caused.clone();
        candidate.session_id = name.to_owned();
        candidate.node_did = owner.to_owned();
        candidate.requester_did = Some(requester.to_owned());
        candidate.caused_by_parent_request_doc_id = parent;
        materialize_session(&self::owner(&message), &candidate).await;
        candidates.push(candidate.session_id);
    }
    candidates.push(caused.session_id.clone());
    let mut args = AgentSessionListArgs {
        limit: Some(1),
        ..Default::default()
    };
    let mut listed = std::collections::BTreeSet::new();
    loop {
        let page = agent_session_list(&node, &caller, &tools, &args)
            .await
            .unwrap();
        for row in page["sessions"].as_array().unwrap() {
            assert_eq!(row["can_message"], true);
            assert!(
                listed.insert(row["session_id"].as_str().unwrap().to_owned()),
                "duplicate paged session"
            );
        }
        args.cursor = page["next_cursor"].as_str().map(ToOwned::to_owned);
        if args.cursor.is_none() {
            break;
        }
    }
    assert!(listed.contains("trigger-worker"));
    for id in candidates {
        assert_eq!(
            listed.contains(&id),
            resolve_send_target(&node, &caller.node_did, &caller.agent_did, &tools, &id)
                .await
                .unwrap()
                .is_some(),
            "{id}"
        );
    }
    let all = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let stale = agent_session_list(
        &node,
        &caller,
        &tools,
        &AgentSessionListArgs {
            cursor: Some("stale cursor".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all, stale);
    let identity = gents_store::identity::KeyIdentity::load_existing(
        message.admission.path.join("node.key"),
        None,
    )
    .unwrap();
    let caller_agent = {
        let agent = gents_documents::load_agent(&node, &caller.agent_did)
            .await
            .unwrap()
            .expect("calling Agent binding");
        gents_store::identity::resolve_agent_identity(
            &identity,
            &gents_protocol::agent_identity::AgentIdentityBinding {
                agent_did: agent.agent_did,
                creation_nonce: agent.creation_nonce,
            },
        )
        .unwrap()
    };
    // A trigger run records the calling Agent as its requester, which is what
    // makes a same-node session without a parent link messageable by it. The
    // run is the calling Agent's own, so its session is the Agent's row.
    let mut worker_session = caused.clone();
    worker_session.session_id = "correlated-worker".to_owned();
    worker_session.agent_did = caller.agent_did.clone();
    worker_session.requester_did = Some(caller.agent_did.clone());
    worker_session.caused_by_parent_request_doc_id = None;
    materialize_session(&owner(&message), &worker_session).await;
    crate::session::recover_agent_session_relationships_with_access(
        &owner(&message),
        &caller.node_did,
        &identity::Did::new(caller.agent_did.as_str()).unwrap(),
    )
    .await
    .unwrap();
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "trigger-worker-request",
        identity.did(),
        caller.agent_did.as_str(),
        caller.agent_did.as_str(),
        "correlated-worker",
        "spawn",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    create.caused_by_trigger_id = Some("shard-assignment".to_owned());
    create.caused_by_trigger_kind = Some("schedule".to_owned());
    create.caused_by_correlation = Some("R3a".to_owned());
    let mut worker =
        crate::tools::call::lifecycle::admission_fixture::claimed_signed_request_create_as_agent(
            &node,
            &identity,
            caller_agent.as_ref(),
            create,
        )
        .await;
    let filtered = agent_session_list(&node, &caller, &tools, &serde_json::from_value(serde_json::json!({"filter": {"caused_by_trigger_id": "shard-assignment", "caused_by_correlation": "R3a", "status": "busy"}})).unwrap()).await.unwrap();
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["sessions"][0]["session_id"], "correlated-worker");
    assert_eq!(filtered["sessions"][0]["relationship"], "same_node");
    assert_eq!(
        filtered["sessions"][0]["request_id"],
        worker.request().request_id
    );
    assert_eq!(filtered["sessions"][0]["lifecycle_state"], "claimed");
    assert_eq!(filtered["sessions"][0]["can_interrupt"], false);
    let target = resolve_send_target(
        &node,
        &caller.node_did,
        &caller.agent_did,
        &tools,
        "correlated-worker",
    )
    .await
    .unwrap()
    .unwrap();
    let mut send_call =
        crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut caller_owner,
            &caller.node_did,
            1,
            gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
            "message-trigger-worker",
            serde_json::json!({"session_id": "correlated-worker", "message": "continue shard R3a"}),
            AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
    let cause = gents_protocol::request_lifecycle::SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: send_call.tool_call_id().to_owned(),
        tool_call_doc_id: send_call.doc_id().unwrap().to_owned(),
        correlation: None,
    };
    let message_plan = plan(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        RenderedBody {
            content: "continue shard R3a".to_owned(),
            goal: None,
        },
        None,
        false,
        &super::hop_tests::signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(message_plan.delivery(), Delivery::Queued);
    let delivered = commit(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &mut send_call,
        message_plan,
        true,
    )
    .await
    .unwrap();
    assert_eq!(delivered.session_id, "correlated-worker");
    assert_eq!(delivered.delivery.as_deref(), Some("queue"));
    let writer = crate::request::streaming::DefraStreamWriter::new(
        node.clone(),
        &caller.node_did,
        std::time::Duration::ZERO,
    );
    worker.begin_owned_execution(&writer).await.unwrap();
    worker
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Interrupted,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    drop(caller_owner);
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

/// A caused session records its opening request when it is created and
/// keeps it when a later request reaches the session.
#[tokio::test]
async fn caused_session_records_its_opening_request() {
    let (message, caller, caused) = started("opening-provenance").await;
    let node = message.admission.node.clone();
    let access = owner(&message);
    let signed = access
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
            escape_graphql_string(&caused.doc_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let mut signed: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(signed["data"]["AgentRequest"][0].clone()).unwrap();
    gents_store::identity::request_receipt::verify_request_receipt_signature(&signed).unwrap();
    signed
        .input
        .as_mut()
        .unwrap()
        .started_by
        .as_mut()
        .unwrap()
        .session_id = "forged".into();
    assert!(
        gents_store::identity::request_receipt::verify_request_receipt_signature(&signed).is_err()
    );
    let provenance = || async {
        crate::session::origin::load_session(
            &access,
            &caused.session_id,
            caused.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap()
        .provenance
        .unwrap()
    };
    let recorded = provenance().await;
    assert_eq!(
        gents_protocol::session::SessionScope::from(recorded.started_by.as_ref().unwrap()),
        gents_protocol::session::SessionScope::of_request(&caller)
    );
    let caller_scope = gents_protocol::session::SessionScope::of_request(&caller);
    let started = crate::session::origin::lineage(&access, &caller_scope)
        .await
        .unwrap()
        .started;
    assert!(started
        .iter()
        .any(|link| link.scope == gents_protocol::session::SessionScope::of_request(&caused)));
    assert_eq!(
        recorded.parent_request_doc_id,
        caused.caused_by_parent_request_doc_id
    );
    assert_eq!(
        recorded.opening_request_doc_id.as_deref(),
        Some(caused.doc_id.as_str())
    );

    let mut later = caused.clone();
    later.doc_id = "bae-later-message".to_owned();
    materialize_session(&owner(&message), &later).await;
    assert_eq!(provenance().await, recorded);

    let mut without_call = caused.clone();
    without_call.caused_by_parent_tool_call_doc_id = None;
    assert_eq!(
        crate::request::lifecycle::materialize::created_session_provenance(&without_call)
            .unwrap()
            .opening_request_doc_id,
        None
    );
    without_call.caused_by_parent_request_doc_id = None;
    assert!(
        crate::request::lifecycle::materialize::created_session_provenance(&without_call).is_none()
    );
    without_call.caused_by_trigger_id = Some("task-trigger".to_owned());
    without_call.caused_by_trigger_kind = Some("event".to_owned());
    assert_eq!(
        crate::request::lifecycle::materialize::created_session_provenance(&without_call)
            .unwrap()
            .opening_request_doc_id
            .as_deref(),
        Some(without_call.doc_id.as_str())
    );
    materialize_session(&owner(&message), &without_call).await;
    assert_eq!(provenance().await, recorded);
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
