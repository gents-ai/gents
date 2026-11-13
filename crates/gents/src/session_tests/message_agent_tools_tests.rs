use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_session::session::message::MessageContent;
use gents_session::session::message::*;
use gents_toolcalls::call::lifecycle::admission_fixture::{owner, set_state, started};

/// A User starts Agent A; A starts Agent B through `agent_new`. B lists A's
/// session, which the User requested, from B's own verified creation receipt;
/// A lists B's; an unrelated Agent C lists neither.

/// The calling Agent's DID, which signs the session-message interrupt checks.

#[tokio::test]
async fn steer_reachability_matches_lean() {
    use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
    let (message, _, head) = started(
        "steer-reachability-model",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    for case in gents_lean_vocab::families::contracts_agent_steer_reachability::cases::<String>() {
        let rows = case
            .input
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
        case.check(
            gents_session::request::lifecycle::queue::steer_reachable(
                &head,
                rows,
                &case.input.target.to_string(),
                chrono::Utc::now(),
            )
            .is_ok(),
        );
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
async fn explicit_steer_checks_the_physical_head_at_publication() {
    let (message, caller, caused) = started(
        "agent-steer-head-change",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = message.admission.node.clone();
    set_state(&owner(&message), &caused.doc_id, "processing").await;
    // Staging verifies the source edge, so the cause must name the accepted tool row.
    let cause =
        gents_toolcalls::call::lifecycle::admission_fixture::cause(&message, caller.request_hop)
            .await;
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
            MessageContent {
                rendered: RenderedBody {
                    content: "change course".to_owned(),
                    goal: None,
                },
                title: None,
            },
            AgentMessageDelivery::Steer,
            &gents_toolcalls::call::lifecycle::admission_fixture::signer(&message),
        )
        .await
        .unwrap()
        .unwrap()
    };
    let stale = make_plan().await;
    set_state(&owner(&message), &caused.doc_id, "completed").await;
    let idle = make_plan().await;
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
        MessageContent {
            rendered: RenderedBody {
                content: "queued turn".to_owned(),
                goal: None,
            },
            title: None,
        },
        AgentMessageDelivery::Queue,
        &gents_toolcalls::call::lifecycle::admission_fixture::signer(&message),
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
    let row = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
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
                gents_session::request::lifecycle::queue::pending_rows_in_txn(
                    txn, node_did, session_id,
                )
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
    for case in gents_lean_vocab::families::contracts_agent_steer_target::cases() {
        let mut head = caused.clone();
        if case.input.planned_head != case.input.active_head {
            head.request_id = "a-later-turn".to_owned();
        }
        case.check(
            gents_session::request::lifecycle::queue::steering_compatible(
                &head,
                candidate,
                chrono::Utc::now(),
            ),
        );
    }
    let publication = |head: &gents_protocol::row::AgentRequest| {
        match gents_session::request::lifecycle::queue::steer_reachable(
            head,
            snapshot.clone(),
            &receipt.doc_id,
            chrono::Utc::now(),
        ) {
            Ok(()) => "steer".to_owned(),
            Err(gents_session::request::lifecycle::queue::SteerAdmissionRefusal::Incompatible) => {
                "refuse".to_owned()
            }
            Err(other) => panic!("steer publication did not reach the active turn: {other}"),
        }
    };
    let planned = |plan: &Plan| match &plan.write {
        gents_session::session::message::plan::PlannedWrite::Request { delivery, .. } => *delivery,
        gents_session::session::message::plan::PlannedWrite::Goal { .. } => {
            panic!("a steer without a Goal planned a Goal request")
        }
    };
    let refusals = contexts
        .iter()
        .map(|(_, head)| publication(head))
        .collect::<Vec<_>>();
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "{refusals:?}"
    );
    for case in gents_lean_vocab::families::contracts_agent_steer_disposition::cases::<String>() {
        let disposition = if !case.input.active {
            assert_eq!(planned(&idle), Delivery::Request);
            "start".to_owned()
        } else {
            assert_eq!(planned(&current), Delivery::Steering);
            if case.input.compatible {
                publication(&caused)
            } else {
                refusals[0].clone()
            }
        };
        case.check(disposition);
    }
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
