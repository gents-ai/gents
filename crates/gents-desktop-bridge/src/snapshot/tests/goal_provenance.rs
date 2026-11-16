use super::*;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity as _;

/// A served runtime after every modeled opening-provenance case has been
/// checked before transcript publication and after reload.
struct GoalProvenance {
    runtime: gents_desktop_core::test_fixtures::ServedRuntimeHome,
    access: ConfigAccess,
    opening_doc: String,
    observed: Vec<AgentRequestRow>,
    applied: std::collections::BTreeMap<String, String>,
}

async fn goal_provenance_cases() -> GoalProvenance {
    // A served runtime whose User published a real provisioned Agent; a
    // guest may write the Agent's sessions.
    let runtime = gents_desktop_core::test_fixtures::ServedRuntimeHome::start().await;
    let binding = runtime.publish_chat_agent().await;
    let identity = runtime.runtime().clone();
    let user = runtime.user().clone();
    let home = tempfile::tempdir().unwrap();
    let guest =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("guest.key"), None)
            .unwrap();
    // The guest is a second enrolled User whom the Agent lets write its
    // sessions.
    gents_store::identity::GrantSet::OperatorUser
        .grant(
            &runtime.runtime_access(),
            &::identity::Did::new(guest.did()).unwrap(),
        )
        .await
        .unwrap();
    let agent = gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
    // The desktop reads its runtime as the User.
    let access = runtime.user_access();
    let agent_doc = runtime.agent_doc_id(&binding).await;
    runtime
        .user_access()
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &::identity::Did::new(guest.did()).unwrap(),
        )
        .await
        .unwrap();
    let reservation = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "reservation",
        identity.did(),
        user.did(),
        &binding.agent_did,
        "session",
        "reserve",
        "interactive",
        "2026-10-25T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let session_doc =
        gents_session::request::lifecycle::reserve_goal_session(&access, &reservation)
            .await
            .unwrap();
    let agent_access = runtime.agent_access(&binding).await;
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let modeled = contract["goal_client_start_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "reserved_session")
        .expect("goal provenance model");
    let mut observed = Vec::new();
    let mut opening_doc = None;
    // Every request the model accepts as an applied opening, by document.
    let mut applied = std::collections::BTreeMap::<String, String>::new();
    for (index, root, alter_assignment, omit_receipt, expected) in [
        (0, None, false, false, Some("goal-0")),
        (1, Some("original-opening"), false, false, None),
        (2, None, true, false, None),
        (3, None, false, true, None),
        (4, None, false, false, None),
        (5, None, false, false, None),
        (6, None, false, false, None),
        (7, None, false, false, None),
        (8, None, false, false, None),
        (9, None, false, false, None),
        (10, None, false, false, None),
        (11, None, false, false, Some("goal-11")),
        (12, None, false, false, None),
    ] {
        let goal_id = format!("goal-{index}");
        let author: &dyn gents_store::identity::NodeIdentity =
            if index >= 11 { &guest } else { user.as_ref() };
        // Each author writes as itself over the runtime's authenticated HTTP.
        let author_access = if index >= 11 {
            ConfigAccess::Graphql(
                gents_store::config_client::GraphqlEndpoint::as_principal(
                    runtime.graphql(),
                    guest.did(),
                )
                .with_client_signing_capability(guest.client_signing_capability().unwrap())
                .unwrap(),
            )
        } else {
            runtime.user_graphql_access()
        };
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("request-{index}"),
            identity.did(),
            if index == 12 { guest.did() } else { user.did() },
            &binding.agent_did,
            "session",
            "Review the changes",
            "interactive",
            "2026-10-25T00:00:00Z",
            AgentRequestAdmissionRecord::client(author.did()),
        );
        create.input.goal_assignment = Some(GoalAssignmentInput {
            goal_id: goal_id.clone(),
            root_request_doc_id: root.map(str::to_owned),
        });
        create.input.queue = Some(
            serde_json::from_value(serde_json::json!({"source":"user", "policy":"append"}))
                .unwrap(),
        );
        create.retry_key = Some(format!("session-goal:command-{index}"));
        create.session_doc_id = Some(session_doc.clone());
        create.agent_doc_id = Some(agent_doc.clone());
        gents_session::request::admission::sign_agent_request_create(author, &mut create)
            .await
            .unwrap();
        let written = author_access
            .write(
                "goal_provenance.fixture",
                &create.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let doc = gents_store::graphql::created_doc_id(&written, "AgentRequest").unwrap();
        // The author links its request to the Agent, the session and the
        // working Agent, as client admission stages a request.
        author_access
            .add_resource_relationship("AgentRequest", &doc, "parent", "agents", &agent_doc)
            .await
            .unwrap();
        author_access
            .add_resource_relationship("AgentRequest", &doc, "session", "sessions", &session_doc)
            .await
            .unwrap();
        author_access
            .add_actor_relationship(
                "AgentRequest",
                &doc,
                "working_agent",
                &::identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        if index == 0 {
            opening_doc = Some(doc.clone());
        }
        let goal_literal = gents_store::graphql::escape_graphql_string(&goal_id);
        let goal_doc = if index != 10 {
            let request_binding = if index == 5 {
                "other-physical-request"
            } else {
                &doc
            };
            let goal = agent_access.write("goal_provenance.goal", &format!(r#"mutation {{ create_Goal(input: {{
                goal_id: "{goal_literal}", node_did: "{}", session_id: "session", creation_key: "{goal_literal}",
                session_doc_id: "{}", agent_did: "{}",
                objective: "Review the changes", status: "active", assignment_root_request_doc_id: "{}",
                created_at: "2026-10-25T00:00:00Z", updated_at: "2026-10-25T00:00:00Z"
            }}) {{_docID}} }}"#, gents_store::graphql::escape_graphql_string(identity.did()),
                gents_store::graphql::escape_graphql_string(&session_doc),
                gents_store::graphql::escape_graphql_string(&binding.agent_did),
                gents_store::graphql::escape_graphql_string(request_binding))).await.unwrap();
            let goal_doc = gents_store::graphql::created_doc_id(&goal, "Goal").unwrap();
            // A Goal is a session child, as the Goal owner links it.
            agent_access
                .transact_write("goal_provenance.goal_parent", |txn| {
                    let (goal_doc, session_doc) = (&goal_doc, &session_doc);
                    Box::pin(async move {
                        txn.add_resource_relationship(
                            "Goal",
                            goal_doc,
                            "parent",
                            "sessions",
                            session_doc,
                        )
                        .await
                    })
                })
                .await
                .unwrap();
            Some(goal_doc)
        } else {
            None
        };
        let mut command = gents_protocol::goal_control::SessionGoalControl {
            version: gents_protocol::goal_control::SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("command-{index}"),
            requester_did: author.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            agent_did: create.agent_did.clone(),
            agent_doc_id: agent_doc.clone(),
            session_id: "session".into(),
            session_doc_id: Some(session_doc.clone()),
            issued_at: "2026-10-25T00:00:00Z".into(),
            expires_at: "2026-10-25T00:01:00Z".into(),
            expected: None,
            operation: gents_protocol::goal_control::GoalOperation::Create {
                objective: create.content.clone(),
                token_budget: None,
            },
            initial_request: Some(Box::new(create.clone())),
            native_evidence: None,
            signature: Vec::new(),
        };
        command.signature = author
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if index == 8 {
            command.signature[0] ^= 1;
        }
        let mut receipt = gents_protocol::goal_control::SessionGoalControlReceipt {
            version: command.version,
            command_id: command.command_id.clone(),
            command_digest: command.computed_digest().unwrap(),
            requester_did: command.requester_did.clone(),
            node_did: command.node_did.clone(),
            agent_did: command.agent_did.clone(),
            session_id: command.session_id.clone(),
            outcome: gents_protocol::goal_control::GoalControlOutcome::Applied {
                goal_id: goal_id.clone(),
                request_id: Some(create.request_id.clone()),
            },
            processed_at: "2026-10-25T00:00:01Z".into(),
            signature: Vec::new(),
        };
        if index == 6 {
            receipt.outcome = gents_protocol::goal_control::GoalControlOutcome::Applied {
                goal_id: goal_id.clone(),
                request_id: Some("other-request".into()),
            };
        }
        if index == 7 {
            receipt.outcome = gents_protocol::goal_control::GoalControlOutcome::Applied {
                goal_id: "other-goal".into(),
                request_id: Some(create.request_id.clone()),
            };
        }
        if index == 9 {
            receipt.session_id = "other-session".into();
        }
        receipt.signature = agent
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        if index == 4 {
            receipt.signature[0] ^= 1;
        }
        let control = author_access
            .write(
                "goal_provenance.control",
                &format!(
                    r#"mutation {{ create_AgentSessionGoalControl(input: {{
            command_id: "{}", requester_did: "{}", node_did: "{}", agent_did: "{}", session_id: "session",
            session_doc_id: "{}", intent_json: "{}"
        }}) {{_docID}} }}"#,
                    gents_store::graphql::escape_graphql_string(&command.command_id),
                    gents_store::graphql::escape_graphql_string(author.did()),
                    gents_store::graphql::escape_graphql_string(identity.did()),
                    gents_store::graphql::escape_graphql_string(&binding.agent_did),
                    gents_store::graphql::escape_graphql_string(&session_doc),
                    gents_store::graphql::escape_graphql_string(
                        &serde_json::to_string(&command).unwrap()
                    )
                ),
            )
            .await
            .unwrap();
        let control_doc =
            gents_store::graphql::created_doc_id(&control, "AgentSessionGoalControl").unwrap();
        author_access
            .add_resource_relationship(
                "AgentSessionGoalControl",
                &control_doc,
                "parent",
                "sessions",
                &session_doc,
            )
            .await
            .unwrap();
        author_access
            .add_actor_relationship(
                "AgentSessionGoalControl",
                &control_doc,
                "working_agent",
                &::identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        if !omit_receipt {
            // The receipt's links are staged with it, as its owner publishes
            // them.
            agent_access.transact_write("goal_provenance.receipt", |txn| Box::pin(async {
                let result = txn.execute_with_variables(
                    "mutation($input: AgentSessionGoalReceiptMutationInputArg!) { create_AgentSessionGoalReceipt(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"command_doc_id": control_doc, "command_id": command.command_id,
                        "receipt_json": serde_json::to_string(&receipt)?}}),
                ).await?;
                let receipt_doc = gents_store::graphql::created_doc_id(&result, "AgentSessionGoalReceipt")?;
                txn.add_resource_relationship(
                    "AgentSessionGoalReceipt",
                    &receipt_doc,
                    "parent",
                    "goal_controls",
                    &control_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentSessionGoalReceipt",
                    &receipt_doc,
                    "reader",
                    &::identity::Did::new(author.did())?,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })).await.unwrap();
        }
        let response = access
            .execute(&format!(
                "{{ AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} }} }}",
                gents_store::graphql::escape_graphql_string(&doc),
                gents::SIGNED_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        let mut request: AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        if alter_assignment {
            request
                .input
                .as_mut()
                .unwrap()
                .goal_assignment
                .as_mut()
                .unwrap()
                .goal_id = "forged-goal".into();
        }
        request.admission_signature = None;
        request.lifecycle_state = Some(RequestLifecycleState::Pending);
        let mut rows = ClientStoreRows {
            sessions: vec![AgentSession {
                session_id: "session".into(),
                node_did: identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: binding.agent_did.clone(),
                created_at: "2026-10-25T00:00:00Z".into(),
                closed_at: None,
                archived_at: None,
                archive_revision: 0,
                stopped_at: None,
                title: None,
                tags: Vec::new(),
                provenance: None,
                observation: None,
            }],
            requests: vec![request],
            ..Default::default()
        };
        let mut snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(rows.clone()),
            "session",
            None,
        )
        .unwrap();
        super::super::session::goal_provenance::attach(
            &access,
            &rows.requests,
            &rows.transcript_messages,
            None,
            &mut snapshot,
        )
        .await
        .expect("goal provenance read");
        let pending = snapshot
            .pending_turn
            .as_ref()
            .expect("pending opening input");
        let expected = if index == 11 {
            modeled["opening_provenance"]["delegated_author"]
                .as_bool()
                .unwrap()
                .then_some("goal-11")
        } else if index == 12 {
            modeled["opening_provenance"]["substituted_requester"]
                .as_bool()
                .unwrap()
                .then_some("goal-12")
        } else {
            expected
        };
        if let Some(goal) = expected {
            applied.insert(doc.clone(), goal.to_owned());
        }
        assert_eq!(
            pending.started_goal_id.as_deref(),
            expected,
            "case {index} pending"
        );
        rows.requests[0].lifecycle_state = Some(RequestLifecycleState::Completed);
        for (suffix, sequence, role) in [
            ("context", 1, MessageRole::User),
            ("prompt", 2, MessageRole::User),
            ("answer", 3, MessageRole::Assistant),
        ] {
            push_canonical_text_message_for_node(
                &mut rows,
                &format!("authored:{doc}:{suffix}"),
                "session",
                Some(&doc),
                sequence,
                role,
                "Review the changes",
                identity.did(),
                Some(if index == 12 { guest.did() } else { user.did() }),
            );
        }
        if index == 0 {
            super::super::session::goal_provenance::tests::assert_copied_opening(&access, &rows)
                .await;
        }
        observed.push(rows.requests[0].clone());
        for reload in 0..2 {
            if index == 0 && reload == 1 {
                runtime
                    .user_graphql_access()
                    .write(
                        "goal_provenance.remove",
                        &format!(
                            "mutation {{ delete_Goal(docID: \"{}\") {{_docID}} }}",
                            gents_store::graphql::escape_graphql_string(goal_doc.as_ref().unwrap()),
                        ),
                    )
                    .await
                    .unwrap();
            }
            let mut snapshot = build_session_snapshot_from_store(
                &ClientStore::from_rows(rows.clone()),
                "session",
                None,
            )
            .unwrap();
            super::super::session::goal_provenance::attach(
                &access,
                &rows.requests,
                &rows.transcript_messages,
                None,
                &mut snapshot,
            )
            .await
            .expect("goal provenance read");
            let markers = snapshot
                .timeline_items
                .iter()
                .filter_map(|item| match item {
                    RenderedTimelineItem::UserMessage {
                        started_goal_id, ..
                    } => started_goal_id.as_deref(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                markers,
                expected.into_iter().collect::<Vec<_>>(),
                "case {index} reload {reload}"
            );
        }
    }
    GoalProvenance {
        runtime,
        access,
        opening_doc: opening_doc.unwrap(),
        observed,
        applied,
    }
}

#[tokio::test]
async fn goal_provenance_agrees_before_transcript_publication_and_after_reload() {
    let GoalProvenance {
        runtime,
        access,
        opening_doc,
        observed,
        applied,
        ..
    } = goal_provenance_cases().await;
    assert_eq!(
        applied.get(&opening_doc).map(String::as_str),
        Some("goal-0")
    );
    let measured = crate::tests::support::CountingRead::new(access);
    let roots = gents_automation::goal::load_applied_opening_goal_ids(
        &measured,
        &observed.iter().collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    // The batched read agrees with the modeled per-case outcomes, including
    // the delegated author the User now reads alongside its own openings.
    assert_eq!(roots, applied);
    assert!(
        measured.queries() <= 2,
        "opening provenance must batch its reads"
    );
    runtime.shutdown().await;
}

/// A Goal opened through the desktop's goal-submission command, followed by
/// ordinary turns a running runtime executes on its scripted provider, pages
/// and forks with the opening's Goal marker. Forks copy their content into
/// destination-owned documents (`SessionFork.publish`); the copied opening
/// still names its origin, so the marker follows it.
#[tokio::test]
async fn paged_native_forks_follow_goal_provenance() {
    use crate::tests::streaming_backend::{StreamPlan, StreamResponse, StreamScript};
    use crate::types::{GoalControlAction, GoalControlRequest};

    let desktop = crate::tests::support::RunningDesktop::start(vec![
        StreamPlan::current_authored_user(
            "GOAL-OPENING",
            vec![StreamResponse::Stream(StreamScript::paused(
                "GOAL-OPENING",
                ["opening answer"],
            ))],
        ),
        StreamPlan::current_authored_user(
            "ORDINARY-TURN",
            vec![StreamResponse::completes(
                "ORDINARY-TURN",
                ["ordinary answer"],
            )],
        ),
    ])
    .await;
    let node = desktop.node_did().to_owned();
    let control = |action| GoalControlRequest {
        client_did: None,
        command_id: None,
        node_did: node.clone(),
        session_id: Some("session".into()),
        session_doc_id: None,
        expected: None,
        agent_did: Some(desktop.lead.agent_did.clone()),
        action,
        cwd: None,
        captured_context: None,
        attachments: None,
    };
    let opened = crate::commands::control_goal(
        &desktop.core,
        control(GoalControlAction::Create {
            objective: "GOAL-OPENING".into(),
            token_budget: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        opened.goal_id,
        gents_protocol::goal::deterministic_goal_id(&node, "session")
    );
    let opening_request = opened.request_id.clone().expect("the opening request");
    let opening = desktop
        .wait_for(
            "the opening turn",
            &format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
                gents_store::graphql::escape_graphql_string(&opening_request)
            ),
            |data| data["AgentRequest"][0]["lifecycle_state"] == "processing",
        )
        .await["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    desktop
        .wait_for(
            "the opening's streamed answer",
            "{ AgentOutputSegment { payload request_doc_id } }",
            |data| {
                data["AgentOutputSegment"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["request_doc_id"] == opening.as_str()
                            && row["payload"]
                                .as_str()
                                .is_some_and(|payload| payload.contains("opening answer"))
                    })
                })
            },
        )
        .await;
    // The User cancels the opening once the runtime holds its answer. An
    // interrupted turn pauses its Goal (`decide_goal_continuation`), so the
    // runtime publishes no continuation between the ordinary turns.
    let interrupted = crate::interrupt::interrupt_request(
        &desktop.core,
        &crate::types::DesktopInterruptRequest {
            request_id: opening_request,
            node_did: Some(node.clone()),
            cause: "userCancelled".into(),
        },
    )
    .await
    .unwrap();
    assert!(interrupted.accepted);
    desktop
        .wait_for_request_state(&opening, "interrupted")
        .await;
    let mut ordinary = Vec::new();
    for index in 0..4 {
        let doc = desktop
            .send(
                "session",
                &format!("ordinary-{index}"),
                &format!("ORDINARY-TURN {index}"),
            )
            .await;
        desktop.wait_for_request_state(&doc, "completed").await;
        ordinary.push(doc);
    }

    let access = desktop.runtime.user_access();
    let messages = access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "session" }}, role: {{ _eq: "user" }} }}) {{ message_key request_doc_id }} }}"#,
            gents_store::graphql::escape_graphql_string(&node)
        ))
        .await
        .unwrap();
    let prompt = |request: &str| {
        let keys = messages["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["request_doc_id"] == request)
            .map(|row| row["message_key"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 1, "{request} publishes one prompt: {messages}");
        keys.into_iter().next().unwrap()
    };
    let session = gents_protocol::session::AgentSession {
        session_id: "session".into(),
        node_did: node.clone(),
        requester_did: Some(desktop.user_did()),
        ..crate::tests::support::fixture_session(&node, "session")
    };
    super::super::session::goal_provenance::tests::assert_paged_native_forks(
        &access,
        &session,
        &opened.goal_id,
        &prompt(&opening),
        &prompt(&ordinary[0]),
    )
    .await;
    desktop.shutdown().await;
}
