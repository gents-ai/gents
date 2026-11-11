use gents_protocol::tool_call::AwaitMode;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, completion_obligations, published_admission,
    published_session_message, published_session_message_with_owner, settle, update_request,
    PublishedAdmissionOptions,
};
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
use std::sync::Arc;

#[tokio::test]
async fn generated_session_message_recovery_cases_use_accepted_call() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.sweep_id == "tool_call_lifecycle_recover_session_message_rows")
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean emitted no session-message recovery rows"
    );
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.pre_state, "running", "{name}");
        let cause = case
            .recovery_cause
            .as_deref()
            .unwrap_or_else(|| panic!("{name}: session-message row carries its observed cause"));
        if cause == "causedRequestUnbound" {
            let admission = published_admission(PublishedAdmissionOptions {
                name: format!("recovery-closeout-{name}"),
                real_identity: true,
                await_mode: AwaitMode::Background,
                tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
                start_running: true,
                ..Default::default()
            })
            .await
            .expect("publish a running session-message row without a receipt");
            let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
            let session_id = admission.tool.session_id().to_owned();
            let settled = settle(&admission).await;
            assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
            let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
            let response = admission
                .tool
                .access()
                .unwrap()
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} }}"#
                ))
                .await
                .unwrap();
            let row = &response["data"]["AgentToolCall"][0];
            assert_eq!(
                row["lifecycle_state"],
                case.terminal_state.as_str(),
                "{name}"
            );
            assert_eq!(row["tool_failure_class"], "external", "{name}");
            let (notifications, _) = completion_obligations(
                &admission.tool.access().unwrap(),
                &session_id,
                &admission.node_did,
                admission.tool.requester_did(),
            )
            .await;
            assert_eq!(
                notifications.len(),
                1,
                "{name}: fails closed with a notification"
            );
            assert_notification_reason(&notifications[0], case);
            assert_eq!(settle(&admission).await, 0, "{name}: fails closed once");
            admission
                .node
                .shutdown()
                .await
                .expect("shut down test node");
            std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
            continue;
        }
        let message = published_session_message(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted session message and materialize its request");
        let admission = &message.admission;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let caused_state = match cause {
            "requestCompleted" => {
                // Each Lean sessionMessageRecoveryCase handoff binds a real
                // Goal observation in the receipt-named caused session:
                // complete (reasonless success form), absent (no Goal at
                // all), unknown (a Goal whose persisted status is malformed,
                // so parsed_status cannot resolve it), and live
                // observations. The fixture states must be distinct or the
                // generated witnesses do not exercise the vocabulary.
                let caused_session_id = {
                    let doc_id =
                        gents_store::graphql::escape_graphql_string(&message.caused_request_doc_id);
                    let response = admission
                        .tool
                        .access()
                        .unwrap()
                        .execute(&format!(
                            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ session_id }} }}"#
                        ))
                        .await
                        .unwrap();
                    response["data"]["AgentRequest"][0]["session_id"]
                        .as_str()
                        .expect("the caused request names its session")
                        .to_owned()
                };
                if name != "session_message_request_completed_no_goal_to_completed" {
                    let access = admission.tool.access().unwrap();
                    let response = access
                        .execute(&format!(
                            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{{}}}}}"#,
                            gents_store::graphql::escape_graphql_string(
                                &message.caused_request_doc_id
                            ),
                            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
                        ))
                        .await
                        .unwrap();
                    let caller: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(response["data"]["AgentRequest"][0].clone())
                            .unwrap();
                    let created = gents_automation::goal::create_goal_for_request_from_access(
                        &admission.node,
                        Some(&access),
                        &caller,
                        "Handoff assignment",
                        None,
                    )
                    .await
                    .expect("bind the modeled Goal to its physical caused request");
                    let goal = created.goal();
                    assert_eq!(
                        goal.assignment_root_request_doc_id.as_deref(),
                        Some(message.caused_request_doc_id.as_str())
                    );
                    let modeled = tokio::task::spawn_blocking(|| {
                        gents_lean_vocab::lean_contract_snapshot()
                            .goal_link_cases
                            .iter()
                            .find(|case| {
                                case.collection == "Goal"
                                    && case.actor == "agent"
                                    && case.mode == "fresh"
                                    && case.commit
                            })
                            .unwrap()
                            .clone()
                    })
                    .await
                    .unwrap();
                    let escaped = gents_store::graphql::escape_graphql_string(&goal.doc_id);
                    let visible = access.execute(&format!(r#"{{ Goal(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ creator_did agent_did parent_linked }} }}"#)).await.unwrap();
                    let selected = caller.agent_did.as_deref().unwrap();
                    assert_eq!(visible["data"]["Goal"][0]["creator_did"], selected);
                    assert_eq!(visible["data"]["Goal"][0]["agent_did"], selected);
                    assert_eq!(
                        visible["data"]["Goal"][0]["parent_linked"],
                        modeled.initial.linked
                    );
                    let owners = access
                        .transact_readonly("test.closeout_goal_owner", |txn| {
                            let doc = &goal.doc_id;
                            Box::pin(async move {
                                txn.document_owners("Goal", std::slice::from_ref(doc)).await
                            })
                        })
                        .await
                        .unwrap();
                    assert_eq!(
                        owners[0].owner.as_ref().map(|owner| owner.as_str()),
                        Some(selected)
                    );
                }
                match name {
                    // The reasonless success form must be produced by an
                    // actually-complete assignment, not by a missing Goal:
                    // those are different production paths.
                    "session_message_request_completed_to_completed" => {
                        gents_automation::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("done"),
                            Some(gents_protocol::goal_status::GoalStatus::Complete),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a complete Goal in the caused session");
                    }
                    // Absent: the caused session carries no Goal document.
                    "session_message_request_completed_no_goal_to_completed" => {}
                    // Unknown: a Goal whose persisted status is malformed, so
                    // the canonical read resolves the document but cannot
                    // parse its status into the vocabulary.
                    "session_message_request_completed_unknown_goal_to_completed" => {
                        let goal = gents_automation::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(gents_protocol::goal_status::GoalStatus::Active),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a Goal in the caused session");
                        seed_goal_status(
                            &admission.tool.access().unwrap(),
                            &goal,
                            r#"status: "corrupted-status""#,
                        )
                        .await;
                    }
                    "session_message_request_completed_live_goal_to_completed" => {
                        gents_automation::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(gents_protocol::goal_status::GoalStatus::Active),
                            None,
                            None,
                        )
                        .await
                        .expect("persist an active Goal in the caused session");
                    }
                    "session_message_request_completed_blocked_goal_to_completed"
                    | "session_message_request_completed_stopped_goal_to_completed" => {
                        gents_automation::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(
                                if name
                                    == "session_message_request_completed_stopped_goal_to_completed"
                                {
                                    gents_protocol::goal_status::GoalStatus::Stopped
                                } else {
                                    gents_protocol::goal_status::GoalStatus::Blocked
                                },
                            ),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a blocked Goal in the caused session");
                    }
                    other => panic!("{other}: unhandled completed session-message case"),
                }
                complete_child_managed_verified(
                    &admission.node,
                    &message.caused_request_id,
                    "done",
                    &admission
                        .tool
                        .access()
                        .unwrap()
                        .managed_agent_signer()
                        .unwrap(),
                    admission.runtime.clone(),
                )
                .await;
                None
            }
            "requestFailed" => Some("failed"),
            "requestDead" => Some("dead"),
            "requestInterrupted" => Some("interrupted"),
            "requestSuperseded" => Some("superseded"),
            other => panic!("{name}: unknown session-message recovery cause {other}"),
        };
        if let Some(state) = caused_state {
            update_request(
                &admission.tool.access().unwrap(),
                &message.caused_request_doc_id,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
        }
        let settled = settle(admission).await;
        assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .tool
            .access()
            .unwrap()
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentToolCall"][0];
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        assert_eq!(row["status"], "completed", "{name}");
        if case.terminal_state == "cancelled" {
            assert_eq!(row["cancel_cause"], "interrupted", "{name}");
        }
        // After settle() the row has converged: settlement persists
        // completionPending:<reason>, then append_background_tool_completion
        // publishes the notice and advances the row to completed (verified in
        // the settlement sequence). The persisted pre-delivery suffix is
        // pinned separately, in the crash-before-notice regression where the
        // side effect is actually skipped.
        assert_eq!(row["status"], "completed", "{name}");
        let session_id = admission.tool.session_id().to_owned();
        let (notifications, _) = completion_obligations(
            &admission.tool.access().unwrap(),
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        assert_eq!(notifications.len(), 1, "{name}");
        // F3: the notification reason must be validated against the modeled
        // handoff for the case, not only against the raw suffix: each
        // generated case pins its own assignment_* vocabulary (or the
        // reasonless success form).
        let expected_reason = handoff_reason_for_case(name);
        assert_eq!(
            case.notification_reason.as_deref(),
            expected_reason,
            "{name}: the Lean contract's notificationReason must match the case's modeled handoff"
        );
        assert_notification_reason(&notifications[0], case);
        assert_eq!(settle(admission).await, 0, "{name}");
        message
            .admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
    }
}

fn assert_notification_reason(notification: &str, case: &gents_lean_vocab::LeanRecoverySweepCase) {
    match case.notification_reason.as_deref() {
        Some(reason) => assert!(
            notification.contains(&format!("<reason>{reason}</reason>")),
            "{}: {notification}",
            case.name
        ),
        None => assert!(!notification.contains("<reason>"), "{}", case.name),
    }
}

/// Corrupt a persisted Goal status through the document mutation path the
/// goal controller tests use, producing an unreadable assignment observation
/// (`parsed_status` resolves the document but cannot parse the status).
async fn seed_goal_status(
    access: &gents_store::config_client::ConfigAccess,
    goal: &gents_automation::goal::GoalDocument,
    fields: &str,
) {
    let doc_id = gents_store::graphql::escape_graphql_string(&goal.doc_id);
    let node_did = gents_store::graphql::escape_graphql_string(&goal.node_did);
    let response = access
        .write(
            "fixture.goal.seed_status",
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .expect("seed Goal status as its owning Agent");
    assert_eq!(
        response["data"]["update_Goal"].as_array().map(Vec::len),
        Some(1),
        "seed Goal status must change the Agent's Goal: {response}"
    );
}

fn handoff_reason_for_case(name: &str) -> Option<&'static str> {
    match name {
        "session_message_request_failed_to_failed" => Some("request_failed"),
        "session_message_request_interrupted_to_cancelled" => Some("request_interrupted"),
        "session_message_request_dead_to_failed" => Some("request_dead"),
        "session_message_request_superseded_to_failed" => Some("request_superseded"),
        "session_message_request_completed_to_completed" => None,
        "session_message_request_completed_no_goal_to_completed" => Some("assignment_absent"),
        "session_message_request_completed_unknown_goal_to_completed" => Some("assignment_unknown"),
        "session_message_request_completed_live_goal_to_completed" => Some("assignment_active"),
        "session_message_request_completed_blocked_goal_to_completed" => Some("assignment_blocked"),
        "session_message_request_completed_stopped_goal_to_completed" => Some("assignment_stopped"),
        other => panic!("{other}: unhandled session-message case"),
    }
}

#[tokio::test]
async fn a_completion_at_the_bound_returns_at_the_callers_hop() {
    use gents_store::identity::NodeIdentity;
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-wake-at-bound".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let signer = message
        .admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    // Node is configuration: its owning User signs the hop bound.
    message
        .admission
        .client
        .transact_write("fixture.node.max_request_hop", |txn| {
            let did = did.as_str();
            Box::pin(async move {
                let (_, mut node_input) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    did,
                    did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("owned Node is unavailable"))?;
                node_input["max_request_hop"] = serde_json::json!(1);
                let plan = gents_documents::DesiredStateApplyPlan::new(vec![
                    gents_documents::DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Node,
                        add: node_input.clone(),
                        update: node_input,
                    },
                ])?;
                gents_documents::apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .expect("raise the node's request hop bound");
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        gents_store::identity::KeyIdentity::load_existing(
            message.admission.path.join("node.key"),
            None,
        )
        .unwrap(),
    );
    let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
    );
    // Wakes and the caller are Agent-owned rows: read them as the Agent.
    let agent_access = message.admission.tool.access().unwrap();
    let pending_wakes = |access: gents_store::config_client::ConfigAccess, session: String| async move {
        let response = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }}, lifecycle_state: {{ _eq: "pending" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(&session)
            ))
            .await
            .unwrap();
        response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let admit = |doc_id: String| {
        let node = node.clone();
        let access = agent_access.clone();
        let verifier = &verifier;
        let signer = &signer;
        async move {
            let request =
                gents_session::request::binding::load_agent_request_by_doc_id_with_access(
                    &access, &doc_id,
                )
                .await
                .unwrap()
                .unwrap();
            let admitted = crate::runtime::agent::daemon::verify_request_at_claim_boundary(
                verifier,
                node.clone(),
                signer,
                request,
            )
            .await;
            let row = access
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop lifecycle_state failure_reason }} }}"#,
                    gents_store::graphql::escape_graphql_string(&doc_id)
                ))
                .await
                .unwrap()["data"]["AgentRequest"][0]
                .clone();
            (admitted.is_some(), row)
        }
    };

    complete_child_managed_verified(
        &node,
        &message.caused_request_id,
        "result at the bound",
        &signer,
        message.admission.runtime.clone(),
    )
    .await;
    assert_eq!(
        gents_toolcalls::background::completion::settle_running_session_message_rows_with_access(
            &node,
            &message.admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
            &did
        )
        .await
        .unwrap(),
        1
    );
    let (notifications, _) = completion_obligations(
        &message.admission.tool.access().unwrap(),
        &session_id,
        &did,
        message.admission.tool.requester_did(),
    )
    .await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("result at the bound"));
    let wakes = pending_wakes(agent_access.clone(), session_id.clone()).await;
    assert_eq!(wakes.len(), 1);

    let caller = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &agent_access,
        &caller_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let native =
        gents_session::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access(
            &message.admission.tool.access().unwrap(),
            &node,
            &caller,
            "process done",
            "background-completion-notification:process:tool",
            gents_protocol::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
            gents_session::request::lifecycle::queue::RequestQueue {
                delivery: Default::default(),
                position: None,
                source: gents_session::request::lifecycle::queue::QueueSource::BackgroundCompletion,
                policy: gents_session::request::lifecycle::queue::QueuePolicy::Coalesce,
                key: Some(format!("background_completion:{session_id}")),
                queued_after_request_id: Some(caller.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            },
            None,
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
        .unwrap();
    assert!(!native.created_request);
    assert_eq!(
        pending_wakes(agent_access.clone(), session_id.clone()).await,
        wakes
    );
    let (admitted, row) = admit(wakes[0].clone()).await;
    assert!(admitted, "{row}");
    assert_eq!(row["request_hop"], caller.request_hop);
    assert_eq!(row["lifecycle_state"], "pending");
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn a_paired_client_session_receives_its_agent_new_result() {
    let (message, mut calling) = published_session_message_with_owner(PublishedAdmissionOptions {
        name: "paired-client-agent-new".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        paired_client: true,
        ..Default::default()
    })
    .await
    .expect("publish a paired client's agent_new and materialize its request");
    let signer = message
        .admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    let identities: Vec<Arc<dyn NodeIdentity>> = vec![Arc::new(signer.clone())];
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    // A paired client's request is visible to its Agent, not to the node.
    let caller = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &caller_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let client = caller
        .requester_did
        .clone()
        .expect("paired client requester");
    assert_ne!(client, did);

    complete_child_managed_verified(
        &node,
        &message.caused_request_id,
        "gatekeeper answer",
        &signer,
        message.admission.runtime.clone(),
    )
    .await;
    assert_eq!(
        gents_toolcalls::background::completion::settle_running_session_message_rows_with_access(
            &node,
            &message.admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
            &did
        )
        .await
        .unwrap(),
        1
    );
    ToolCallLifecycle::reconcile_background_completion_side_effects(&node, &did, &identities)
        .await
        .unwrap();
    let agent_access = message.admission.tool.access().unwrap();
    let response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(&session_id)
        ))
        .await
        .unwrap();
    let wakes = response["data"]["AgentRequest"].as_array().unwrap().clone();
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    let wake = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &agent_access,
        wakes[0]["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wake.requester_did.as_deref(), Some(client.as_str()));
    assert_eq!(wake.request_hop, caller.request_hop);
    calling
        .terminalize_owned(
            gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: message
                    .admission
                    .tool
                    .accepted_header_doc_id()
                    .expect("accepted caller assistant header")
                    .to_owned(),
            },
            None,
        )
        .await
        .unwrap();

    let identity: Arc<dyn NodeIdentity> = Arc::new(
        gents_store::identity::KeyIdentity::load_existing(
            message.admission.path.join("node.key"),
            None,
        )
        .unwrap(),
    );
    let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
    );
    let (verified, client_claim) = crate::runtime::agent::daemon::verify_request_at_claim_boundary(
        &verifier,
        node.clone(),
        &signer,
        wake.clone(),
    )
    .await
    .expect("the returned result is admitted into the client's session");
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        verified.clone(),
        60,
    );
    lifecycle.bind_managed_agent(&signer).unwrap();
    lifecycle.set_client_claim_admission(client_claim);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    gents_toolcalls::call::hook::DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session_id,
        &did,
        verified.requester_did.as_deref(),
        identity::Did::new(&wake.agent_did).unwrap(),
        gents_toolcalls::call::hook::FailurePolicy::FailClosed,
    )
    .await
    .expect("the result resumes the client's session");
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

