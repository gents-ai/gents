use super::*;
use gents::store::config_client::ConfigAccess;
use gents::NodeIdentity as _;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::sync::Arc;

#[tokio::test]
async fn goal_provenance_agrees_before_transcript_publication_and_after_reload() {
    let home = tempfile::tempdir().unwrap();
    let identity = gents::KeyIdentity::load_or_create(home.path().join("node.key"), None).unwrap();
    let user = gents::KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let guest = gents::KeyIdentity::load_or_create(home.path().join("guest.key"), None).unwrap();
    let binding = gents::store::identity::provision_agent_identity(&identity).unwrap();
    let agent = gents::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let node = Arc::new(
        gents::defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local_as(node.clone(), ::identity::Did::new(user.did()).unwrap());
    let (node_doc, agent_doc) = access.transact_write("goal_provenance.identities", |txn| Box::pin(async {
        let node_row = txn.execute_with_variables(
            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
            &serde_json::json!({"input": {"node_did": identity.did()}}),
        ).await?;
        let node_doc = gents::store::graphql::created_doc_id(&node_row, "Node")?;
        let agent_row = txn.execute_with_variables(
            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
            &serde_json::json!({"input": {"node_did": identity.did(), "agent_did": binding.agent_did,
                "creation_nonce": binding.creation_nonce}}),
        ).await?;
        let agent_doc = gents::store::graphql::created_doc_id(&agent_row, "Agent")?;
        Ok::<_, anyhow::Error>((node_doc, agent_doc))
    })).await.unwrap();
    access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &::identity::Did::new(&binding.agent_did).unwrap(),
        )
        .await
        .unwrap();
    access
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
    let session_doc = gents::request::lifecycle::reserve_goal_session(&access, &reservation)
        .await
        .unwrap();
    let agent_access = ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(&binding.agent_did).unwrap(),
    );
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let modeled = contract["goal_client_start_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "reserved_session")
        .expect("goal provenance model");
    let mut observed = Vec::new();
    let mut opening_doc = None;
    let mut opening_rows = None;
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
        let author: &dyn gents::NodeIdentity = if index >= 11 { &guest } else { &user };
        let author_access =
            ConfigAccess::local_as(node.clone(), ::identity::Did::new(author.did()).unwrap());
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
        gents::sign_agent_request_create(author, &mut create)
            .await
            .unwrap();
        let written = author_access
            .write(
                "goal_provenance.fixture",
                &create.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let doc = gents::store::graphql::created_doc_id(&written, "AgentRequest").unwrap();
        if index == 0 {
            opening_doc = Some(doc.clone());
        }
        let goal_literal = gents::store::graphql::escape_graphql_string(&goal_id);
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
            }}) {{_docID}} }}"#, gents::store::graphql::escape_graphql_string(identity.did()),
                gents::store::graphql::escape_graphql_string(&session_doc),
                gents::store::graphql::escape_graphql_string(&binding.agent_did),
                gents::store::graphql::escape_graphql_string(request_binding))).await.unwrap();
            Some(gents::store::graphql::created_doc_id(&goal, "Goal").unwrap())
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
                    gents::store::graphql::escape_graphql_string(&command.command_id),
                    gents::store::graphql::escape_graphql_string(author.did()),
                    gents::store::graphql::escape_graphql_string(identity.did()),
                    gents::store::graphql::escape_graphql_string(&binding.agent_did),
                    gents::store::graphql::escape_graphql_string(&session_doc),
                    gents::store::graphql::escape_graphql_string(
                        &serde_json::to_string(&command).unwrap()
                    )
                ),
            )
            .await
            .unwrap();
        let control_doc =
            gents::store::graphql::created_doc_id(&control, "AgentSessionGoalControl").unwrap();
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
            let receipt_doc = agent_access.transact_write("goal_provenance.receipt", |txn| Box::pin(async {
                let result = txn.execute_with_variables(
                    "mutation($input: AgentSessionGoalReceiptMutationInputArg!) { create_AgentSessionGoalReceipt(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"command_doc_id": control_doc, "command_id": command.command_id,
                        "receipt_json": serde_json::to_string(&receipt)?}}),
                ).await?;
                gents::store::graphql::created_doc_id(&result, "AgentSessionGoalReceipt")
            })).await.unwrap();
            agent_access
                .add_resource_relationship(
                    "AgentSessionGoalReceipt",
                    &receipt_doc,
                    "parent",
                    "goal_controls",
                    &control_doc,
                )
                .await
                .unwrap();
            agent_access
                .add_actor_relationship(
                    "AgentSessionGoalReceipt",
                    &receipt_doc,
                    "reader",
                    &::identity::Did::new(author.did()).unwrap(),
                )
                .await
                .unwrap();
        }
        let response = access
            .execute(&format!(
                "{{ AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} }} }}",
                gents::store::graphql::escape_graphql_string(&doc),
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
            opening_rows = Some(rows.clone());
        }
        observed.push(rows.requests[0].clone());
        for reload in 0..2 {
            if index == 0 && reload == 1 {
                access
                    .write(
                        "goal_provenance.remove",
                        &format!(
                            "mutation {{ delete_Goal(docID: \"{}\") {{_docID}} }}",
                            gents::store::graphql::escape_graphql_string(
                                goal_doc.as_ref().unwrap()
                            ),
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
    super::super::session::goal_provenance::tests::assert_paged_native_forks(
        &access,
        &opening_rows.unwrap(),
        &observed[1],
    )
    .await;
    let measured = crate::tests::support::CountingRead::new(access);
    let roots =
        gents::goal::load_applied_opening_goal_ids(&measured, &observed.iter().collect::<Vec<_>>())
            .await
            .unwrap();
    assert_eq!(
        roots,
        std::collections::BTreeMap::from([(opening_doc.unwrap(), "goal-0".into())])
    );
    assert!(
        measured.queries() <= 2,
        "opening provenance must batch its reads"
    );
    node.shutdown().await.unwrap();
}
