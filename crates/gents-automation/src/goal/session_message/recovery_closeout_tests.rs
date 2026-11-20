use super::SessionGoals;
use gents_protocol::tool_call::AwaitMode;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, completion_obligations, published_admission,
    published_session_message, settle_with, update_request, PublishedAdmissionOptions,
};

#[tokio::test]
async fn generated_session_message_recovery_cases_use_accepted_call() {
    for case in gents_lean_vocab::families::recovery_sweeps_session_message::cases() {
        let input = &case.input;
        let name = input.name.as_str();
        let cause = input.recovery_cause.as_str();
        let assignment = input.assignment.as_str();
        if cause == "causedRequestUnbound" {
            let admission = published_admission(PublishedAdmissionOptions {
                name: format!("recovery-closeout-{name}"),
                real_identity: true,
                await_mode: AwaitMode::Background,
                tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
                start_running: true,
                composition: crate::lifecycle_hooks::composition(),
                ..Default::default()
            })
            .await
            .expect("publish a running session-message row without a receipt");
            let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
            let session_id = admission.tool.session_id().to_owned();
            assert_eq!(
                lifecycle_state(&admission, &tool_doc_id).await,
                input.pre_state,
                "{name}"
            );
            let first = settle_with(&admission, &SessionGoals).await;
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
            let second = settle_with(&admission, &SessionGoals).await;
            let observed = settled(row, first, second, &notifications);
            admission
                .node
                .shutdown()
                .await
                .expect("shut down test node");
            std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
            case.check(observed);
            continue;
        }
        let message = published_session_message(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            composition: crate::lifecycle_hooks::composition(),
            ..Default::default()
        })
        .await
        .expect("publish accepted session message and materialize its request");
        let admission = &message.admission;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        assert_eq!(
            lifecycle_state(admission, &tool_doc_id).await,
            input.pre_state,
            "{name}"
        );
        let caused_state = match cause {
            "requestCompleted" => {
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
                if assignment != "absent" {
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
                    let created = crate::goal::create_goal_for_request_from_access(
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
                    let escaped = gents_store::graphql::escape_graphql_string(&goal.doc_id);
                    let visible = access.execute(&format!(r#"{{ Goal(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ creator_did agent_did parent_linked }} }}"#)).await.unwrap();
                    let selected = caller.agent_did.as_deref().unwrap();
                    assert_eq!(visible["data"]["Goal"][0]["creator_did"], selected);
                    assert_eq!(visible["data"]["Goal"][0]["agent_did"], selected);
                    for case in gents_lean_vocab::families::goal_links_closeout_goal::cases() {
                        let collection = case.input.collection.as_str();
                        case.check(
                            visible["data"][collection][0]["parent_linked"]
                                .as_bool()
                                .unwrap(),
                        );
                    }
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
                match assignment {
                    // The reasonless success form must be produced by an
                    // actually-complete assignment, not by a missing Goal:
                    // those are different production paths.
                    "complete" => {
                        crate::goal::seed_goal_from_access(
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
                    "absent" => {}
                    // Unknown: a Goal whose persisted status is malformed, so
                    // the canonical read resolves the document but cannot
                    // parse its status into the vocabulary.
                    "unknown" => {
                        let goal = crate::goal::seed_goal_from_access(
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
                    "active" => {
                        crate::goal::seed_goal_from_access(
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
                    "blocked" | "stopped" => {
                        crate::goal::seed_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(if assignment == "stopped" {
                                gents_protocol::goal_status::GoalStatus::Stopped
                            } else {
                                gents_protocol::goal_status::GoalStatus::Blocked
                            }),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a blocked Goal in the caused session");
                    }
                    other => panic!("{name}: no fixture binds assignment {other}"),
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
        let first = settle_with(admission, &SessionGoals).await;
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
        assert_eq!(row["status"], "completed", "{name}");
        if row["lifecycle_state"] == "cancelled" {
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
        let second = settle_with(admission, &SessionGoals).await;
        let observed = settled(row, first, second, &notifications);
        message
            .admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
        case.check(observed);
    }
}

async fn lifecycle_state(
    admission: &gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission,
    tool_doc_id: &str,
) -> String {
    let escaped = gents_store::graphql::escape_graphql_string(tool_doc_id);
    let response = admission
        .tool
        .access()
        .unwrap()
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentToolCall"][0]["lifecycle_state"]
        .as_str()
        .expect("the tool row has a lifecycle state")
        .to_owned()
}

fn settled(
    row: &serde_json::Value,
    first: usize,
    second: usize,
    notifications: &[String],
) -> gents_lean_vocab::families::recovery_sweeps_session_message::SessionMessageSettled {
    let [notice] = notifications else {
        panic!("one completion notice per settled row: {notifications:?}");
    };
    gents_lean_vocab::families::recovery_sweeps_session_message::SessionMessageSettled {
        terminal_state: row["lifecycle_state"]
            .as_str()
            .expect("the tool row has a lifecycle state")
            .to_owned(),
        measure_before: u64::try_from(first).expect("a settlement count fits u64"),
        measure_after: u64::try_from(second).expect("a settlement count fits u64"),
        notification_reason: notice
            .split_once("<reason>")
            .and_then(|(_, rest)| rest.split_once("</reason>"))
            .map(|(reason, _)| reason.to_owned()),
    }
}

/// Corrupt a persisted Goal status through the document mutation path the
/// goal controller tests use, producing an unreadable assignment observation
/// (`parsed_status` resolves the document but cannot parse the status).
async fn seed_goal_status(
    access: &gents_store::config_client::ConfigAccess,
    goal: &crate::goal::GoalDocument,
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
