use super::*;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};

async fn fixture_session(
    access: &ConfigAccess,
    node_did: &str,
    user_did: &str,
    agent_did: &str,
    agent_doc: &str,
    session_id: &str,
) -> String {
    access.transact_write("test.first_binding.session", |txn| {
        Box::pin(async move {
            let input = serde_json::json!({"node_did": node_did, "session_id": session_id,
                "requester_did": user_did, "agent_did": agent_did,
                "relationships_pending": false, "created_at": "2026-11-04T00:00:00Z"});
            let created = txn.execute_client_signed_create(
                "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                &serde_json::json!({"input": input}), gents_protocol::schemas::AGENT_SESSION,
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&created, "AgentSession")?;
            txn.add_resource_relationship("AgentSession", &doc, "parent", "agents", agent_doc).await?;
            txn.add_actor_relationship("AgentSession", &doc, "working_agent", &identity::Did::new(agent_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap()
}

async fn fixture_goal(access: &ConfigAccess, node_did: &str, session_id: &str) -> GoalDocument {
    access
        .transact_readonly("test.first_binding.goal", |txn| {
            Box::pin(load_canonical_goal_in_txn(txn, node_did, session_id))
        })
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn generated_continuation_assignments_preserve_physical_bindings() {
    gents_lean_vocab::families::goal_first_request_binding_continuation::replay(|case| {
        let parent: Option<GoalAssignmentInput> =
            case.parent.as_ref().map(gents_lean_vocab::family::recode);
        gents_lean_vocab::family::recode(&continuation_reference(
            &case.goal,
            case.current.as_deref(),
            &case.parent_doc,
            parent.as_ref(),
            case.receipt_doc.as_deref(),
        ))
    });
}

#[test]
fn generated_continuation_edges_accept_only_the_parent_assignment() {
    gents_lean_vocab::families::goal_first_request_binding_edge::replay(|case| {
        let parent: Option<GoalAssignmentInput> =
            case.parent.as_ref().map(gents_lean_vocab::family::recode);
        let child: Option<GoalAssignmentInput> =
            case.child.as_ref().map(gents_lean_vocab::family::recode);
        gents_lean_vocab::families::goal_first_request_binding_edge::EdgeOutput {
            accepted: continuation_reference_matches(
                &case.goal,
                &case.parent_doc,
                parent.as_ref(),
                child.as_ref(),
            ),
        }
    });
}

#[tokio::test]
async fn generated_first_request_bindings_commit_exact_physical_roots() {
    let tasks = gents_lean_vocab::families::goal_first_request_binding_task::cases();
    let bindings = gents_lean_vocab::families::goal_first_request_binding_binding::cases();
    assert_eq!(bindings.len(), tasks.len());
    for (binding_case, task_case) in bindings.into_iter().zip(tasks) {
        let case = &binding_case.input;
        let task = &task_case.input;
        assert_eq!(case.name, task.name);
        let db = gents_inference::test_support::test_db(
            "first-request-binding",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let signer = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let temp = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
        gents_store::identity::GrantSet::OperatorUser
            .grant(
                &gents_store::config_client::ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(signer.did()).unwrap(),
                ),
                &identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
        let binding = gents_store::identity::provision_agent_identity(&signer).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&signer, &binding).unwrap();
        let access = db.client_access(&user);
        let agent_doc = access.transact_write("test.first_binding.agent", |txn| {
            Box::pin(async {
                let created = txn.execute_client_signed_create(
                    "mutation($input: AgentMutationInputArg!) {create_Agent(input: $input) {_docID}}",
                    &serde_json::json!({"input": {"node_did": signer.did(), "agent_did": agent.did(),
                        "creation_nonce": binding.creation_nonce, "display_name": "test agent", "inference_profile_id": "test-profile"}}),
                    gents_protocol::schemas::AGENT,
                ).await?;
                let doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
                txn.add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(agent.did())?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        }).await.unwrap();
        let session_doc = fixture_session(
            &access,
            signer.did(),
            user.did(),
            agent.did(),
            &agent_doc,
            "session",
        )
        .await;
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            signer.did(),
            user.did(),
            agent.did(),
            "session",
            "Start work",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        create.session_doc_id = Some(session_doc.clone());
        create.agent_doc_id = Some(agent_doc.clone());
        gents_session::request::admission::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        gents_session::request::lifecycle::submit_client_request(&access, &create)
            .await
            .unwrap();
        let request = access
            .execute(r#"{AgentRequest(filter: {request_id: {_eq: "first"}}) {_docID}}"#)
            .await
            .unwrap();
        let physical = request["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        super::super::create_goal_for_session_from_access(
            &agent_access,
            signer.did(),
            "session",
            "Start work",
            None,
        )
        .await
        .unwrap();
        let mut goal = fixture_goal(&access, signer.did(), "session").await;
        let root = |value: &str| {
            if value == "request" {
                physical.clone()
            } else {
                value.to_owned()
            }
        };
        if let Some(current) = case.current.as_deref() {
            let doc = escape_graphql_string(&goal.doc_id);
            let current = root(current);
            access.transact_write("test.first_binding.previous_root", |txn| {
                let current = &current;
                let doc = &doc;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        &format!(r#"mutation($input: GoalMutationInputArg!) {{update_Goal(docID: "{doc}", input: $input) {{_docID}}}}"#),
                        &serde_json::json!({"input": {"assignment_root_request_doc_id": current}}),
                        gents_protocol::schemas::GOAL,
                    ).await
                })
            }).await.unwrap();
            goal = fixture_goal(&access, signer.did(), "session").await;
        }
        let mut reference: Option<GoalAssignmentInput> = case
            .reference
            .as_ref()
            .map(gents_lean_vocab::family::recode);
        if let Some(reference) = reference.as_mut() {
            if reference.goal_id == "goal" {
                reference.goal_id = goal.goal_id.clone();
            }
        }
        if case.goal.is_empty() {
            goal.goal_id.clear();
        }
        let request_doc = root(case.request_doc.as_str());
        let accepted = std::sync::atomic::AtomicBool::new(false);
        let result = access
            .transact_write("test.first_binding.transaction", |txn| {
                let goal = &goal;
                let request_doc = &request_doc;
                let reference = reference.as_ref();
                let accepted = &accepted;
                Box::pin(async move {
                    let bound =
                        bind_goal_first_request_in_txn(txn, goal, request_doc, reference).await;
                    accepted.store(bound.is_ok(), std::sync::atomic::Ordering::SeqCst);
                    bound?;
                    anyhow::ensure!(case.commit, "injected publication discard");
                    Ok(())
                })
            })
            .await;
        let accepted = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            result.is_ok(),
            accepted && case.commit,
            "{}: {result:?}",
            case.name
        );
        let after = fixture_goal(&access, signer.did(), "session").await;
        binding_case.check(
            gents_lean_vocab::families::goal_first_request_binding_binding::BindingOutput {
                accepted,
                root: after.assignment_root_request_doc_id.map(|value| {
                    if value == physical {
                        "request".to_owned()
                    } else {
                        value
                    }
                }),
            },
        );

        let task_session = "task-session";
        fixture_session(
            &access,
            signer.did(),
            user.did(),
            agent.did(),
            &agent_doc,
            task_session,
        )
        .await;
        let initial = deterministic_goal_id(signer.did(), task_session);
        let task_id = |value: &str| {
            if value == "initial-goal" {
                initial.clone()
            } else {
                value.to_owned()
            }
        };
        let selected = task_assignment_goal_id(
            task.current_task_goal.as_deref().map(task_id).as_deref(),
            signer.did(),
            task_session,
        );
        if let Some(observed) = task.observed_task_goal.as_deref() {
            let identity = GoalCreationIdentity {
                goal_id: task_id(observed),
                creation_key: format!("task-create:{}", case.name),
            };
            access
                .transact_write("test.first_binding.task_goal", |txn| {
                    let identity = &identity;
                    let signer = &signer;
                    Box::pin(async move {
                        super::super::stage_goal_and_claim(
                            txn,
                            signer.did(),
                            task_session,
                            "Task work",
                            None,
                            GoalState {
                                status: GoalStatus::Active,
                                blocked_audits: 0,
                                wrapup_requested: false,
                                wrapup_completed: false,
                            },
                            "2026-10-21T00:00:00Z",
                            Some(identity),
                            None,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
        }
        let task_reference = GoalAssignmentInput {
            goal_id: selected.clone(),
            root_request_doc_id: None,
        };
        let checked = access
            .transact_readonly("test.first_binding.task_identity", |txn| {
                let signer = &signer;
                let selected = &selected;
                let task_reference = &task_reference;
                Box::pin(async move {
                    validate_task_goal_identity_in_txn(
                        txn,
                        signer.did(),
                        task_session,
                        selected,
                        Some(task_reference),
                    )
                    .await
                })
            })
            .await;
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let fire_identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: signer.did().to_owned(),
            trigger_id: "manual:task".into(),
            source_collection: "Task".into(),
            source_doc_id: "task-binding".into(),
        };
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            fire_key: fire_identity.fire_key(),
            request_id: fire_identity.request_id(),
            identity: fire_identity,
            task_id: "task".into(),
            session_id: task_session.into(),
            goal_id: Some(selected.clone()),
            goal_objective: Some("Task work".into()),
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-10-21T00:00:00Z".into(),
        };
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &fire.request_id,
            signer.did(),
            user.did(),
            agent.did(),
            task_session,
            "Task work",
            "scheduled",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(agent.did(), "manual:task"),
        );
        request.caused_by_trigger_id = Some("manual:task".into());
        request.caused_by_trigger_kind = Some("schedule".into());
        request.input.goal_assignment = Some(task_reference);
        gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut request)
            .await
            .unwrap();
        let admitted =
            crate::triggers::write_task_delivery(&agent_access, &fire, true, &request).await;
        assert_eq!(
            admitted.is_ok(),
            checked.is_ok(),
            "{}: admission error {:?} after identity check {checked:?}",
            case.name,
            admitted.as_ref().err()
        );
        task_case.check(
            gents_lean_vocab::families::goal_first_request_binding_task::TaskOutput {
                task_goal: if selected == initial {
                    "initial-goal".to_owned()
                } else {
                    selected.clone()
                },
                task_binding_valid: checked.is_ok(),
            },
        );
        if let Ok(admitted) = admitted {
            let persisted =
                gents_session::request::binding::load_agent_request_by_doc_id_with_access(
                    &agent_access,
                    &admitted.request.doc_id,
                )
                .await
                .unwrap()
                .unwrap();
            let mut registry = gents_session::request::hooks::registry::Registry::default();
            gents_session::request::hooks::operations::register(&mut registry);
            gents_tools::native::register_lifecycle_reads(&mut registry);
            crate::triggers::lifecycle_hooks::register(&mut registry);
            crate::goal::lifecycle_hooks::register(&mut registry);
            agent_access
                .transact_write("test.first_binding.task_claim", |txn| {
                    let persisted = &persisted;
                    let registry = &registry;
                    Box::pin(async move {
                        gents_session::request::hooks::points::claim_applied_in(
                            txn,
                            registry,
                            persisted,
                            "2026-10-21T00:00:01Z",
                        )
                        .await
                    })
                })
                .await
                .unwrap();
            let assigned = fixture_goal(&agent_access, signer.did(), task_session).await;
            assert_eq!(assigned.goal_id, selected);
            assert_eq!(
                assigned.assignment_root_request_doc_id.as_deref(),
                Some(admitted.request.doc_id.as_str())
            );
            assert_eq!(
                persisted.input.goal_assignment,
                request.input.goal_assignment
            );
        } else {
            let state = agent_access
                .execute("{TriggerFire {fire_key} AgentRequest {session_id}}")
                .await
                .unwrap();
            assert!(state["data"]["TriggerFire"].as_array().unwrap().is_empty());
            assert!(!state["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["session_id"] == task_session));
        }
        node.shutdown().await.expect("node shutdown");
    }
}
