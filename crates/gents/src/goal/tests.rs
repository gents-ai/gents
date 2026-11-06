use super::*;

#[tokio::test]
async fn session_token_usage_breakdown_sums_to_the_total() {
    use crate::store::identity::NodeIdentity;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    use serde_json::json;

    let (db, user, user_access, binding) =
        crate::test_support::signed_test_node("goal-session-usage").await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let owner = db.node_identity.did();
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "usage-request",
        owner,
        user.did(),
        agent.did(),
        "usage-session",
        "test",
        "interactive",
        "2026-07-15T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &db.node,
            &user_access,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    let request_doc_id = lifecycle.request().doc_id.clone();
    let session_doc_id = user_access
        .transact_readonly("test.goal_usage.session", |txn| {
            Box::pin(async move {
                crate::session::load_agent_session_owner_in_txn(txn, owner, "usage-session").await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .doc_id;
    let agent_access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    for (seq, prompt, completion) in [(1, 1000, 50), (2, 200, 7)] {
        let input = json!({
            "call_id": format!("usage-call-{seq}"), "runtime_instance_id": "goal-test",
            "request_id": "usage-request", "request_doc_id": request_doc_id,
            "session_id": "usage-session", "call_seq": seq, "backend_id": "backend-test",
            "agent_did": agent.did(), "node_did": owner, "call_kind": "inference", "attempt": 1,
            "call_state": "completed", "queued_at": "2026-07-15T00:00:00Z",
            "started_at": "2026-07-15T00:00:00Z", "ended_at": "2026-07-15T00:00:01Z",
            "priority": 0, "queue_depth_at_enqueue": 0, "controller_generation": 1,
            "backend_config_fingerprint": "goal-test", "prompt_tokens": prompt,
            "completion_tokens": completion,
        });
        agent_access.transact_write("test.goal_usage.call", |txn| {
            let input = &input;
            let session_doc_id = &session_doc_id;
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) { _docID } }",
                    &json!({"input": input}),
                ).await?;
                let doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
                txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc_id).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    }
    let usage = agent_access
        .transact_readonly("goal.session_usage_breakdown_test", |txn| {
            Box::pin(async move {
                session_token_usage_breakdown_in_txn(txn, owner, "usage-session").await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        usage,
        SessionTokenUsage {
            input: 1200,
            output: 57,
            total: 1257
        }
    );
    assert_eq!(usage.input + usage.output, usage.total);
    assert_eq!(
        session_token_usage(
            &db.node,
            &identity::Did::new(agent.did()).unwrap(),
            owner,
            "usage-session"
        )
        .await
        .unwrap(),
        1257
    );
    let calls = user_access
        .execute("{ InferenceCall { _docID } }")
        .await
        .unwrap();
    assert_eq!(calls["data"]["InferenceCall"].as_array().unwrap().len(), 2);
}

async fn auto_resume_access() -> (crate::ConfigAccess, String) {
    use crate::NodeIdentity;
    let directory = tempfile::tempdir().unwrap();
    let root = crate::KeyIdentity::load_or_create(directory.path().join("root.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let node = std::sync::Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node,
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    for session in ["session", "s"] {
        access
            .transact_write("test.auto_resume_session", |txn| {
                Box::pin(crate::session::ensure_session_in_txn(
                    txn,
                    session,
                    root.did(),
                    agent.did(),
                    Some(agent.did()),
                    None,
                    None,
                    "2026-11-04T00:00:00Z",
                ))
            })
            .await
            .unwrap();
    }
    (access, root.did().to_owned())
}

#[tokio::test]
async fn auto_resume_field_round_trips_through_the_live_schema() {
    let (access, owner) = auto_resume_access().await;
    let goal = set_goal_from_access(&access, &owner, "session", Some("ship"), None, None, None)
        .await
        .unwrap();
    assert_eq!(goal.auto_resume_at_reset, None);
    let doc = escape_graphql_string(&goal.doc_id);
    access.write("test.auto_resume.set", &format!(r#"mutation {{ update_Goal(docID: "{doc}", input: {{auto_resume_at_reset: true}}) {{_docID}} }}"#)).await.unwrap();
    let stored = access
        .transact_readonly("test.auto_resume.read", |txn| {
            Box::pin(load_canonical_goal_in_txn(txn, &owner, "session"))
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.auto_resume_at_reset, Some(true));
}

#[tokio::test]
async fn auto_resume_operator_setter_stores_it() {
    let (access, owner) = auto_resume_access().await;
    let created = set_goal_from_access(&access, &owner, "s", Some("ship"), None, None, Some(true))
        .await
        .unwrap();
    assert_eq!(created.auto_resume_at_reset, Some(true));
    let off = set_goal_from_access(&access, &owner, "s", None, None, None, Some(false))
        .await
        .unwrap();
    assert_eq!(off.auto_resume_at_reset, Some(false));
}

#[tokio::test]
async fn auto_resume_none_and_set_goal_keep_the_stored_value() {
    let (access, owner) = auto_resume_access().await;
    set_goal_from_access(&access, &owner, "s", Some("ship"), None, None, Some(true))
        .await
        .unwrap();
    let kept = set_goal_from_access(
        &access,
        &owner,
        "s",
        None,
        Some(GoalStatus::Paused),
        Some(Some(10)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(kept.auto_resume_at_reset, Some(true));
    let unchanged = set_goal_from_access(&access, &owner, "s", Some("ship more"), None, None, None)
        .await
        .unwrap();
    assert_eq!(unchanged.auto_resume_at_reset, Some(true));
}

#[tokio::test]
async fn session_goal_delete_uses_selected_agent_native_owner() {
    use crate::store::identity::NodeIdentity;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, user_access, binding) =
        crate::test_support::signed_test_node("goal-selected-agent-delete").await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let admin = crate::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    admin
        .grant_node_permission(
            &identity::Did::new(agent.did()).unwrap(),
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "delete-request",
        db.node_identity.did(),
        user.did(),
        agent.did(),
        "delete-session",
        "test",
        "interactive",
        "2026-07-15T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let _lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &db.node,
            &user_access,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    let agent_access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let goal = set_goal_from_access(
        &agent_access,
        db.node_identity.did(),
        "delete-session",
        Some("test deletion"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    agent_access
        .add_actor_relationship(
            "Goal",
            &goal.doc_id,
            "reader",
            &identity::Did::new(db.node_identity.did()).unwrap(),
        )
        .await
        .unwrap();
    let runtime_access = crate::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let denied = delete_goal_from_access(&runtime_access, &goal).await;
    assert!(
        denied.is_err(),
        "runtime identity must not delete the selected agent's goal"
    );
    assert!(delete_goal_from_access(&agent_access, &goal).await.unwrap());
    assert!(
        agent_access.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn held_user_deletes_own_goal_and_claim_by_physical_id() {
    use crate::store::identity::NodeIdentity;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, user_access, binding) =
        crate::test_support::signed_test_node("goal-user-delete").await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let admin = crate::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    admin
        .grant_node_permission(
            &identity::Did::new(user.did()).unwrap(),
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "user-delete-request",
        db.node_identity.did(),
        user.did(),
        agent.did(),
        "user-delete-session",
        "test",
        "interactive",
        "2026-07-15T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let _lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &db.node,
            &user_access,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    set_goal_from_access(
        &user_access,
        db.node_identity.did(),
        "user-delete-session",
        Some("user-owned goal"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let other_create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "other-delete-request",
        db.node_identity.did(),
        user.did(),
        agent.did(),
        "other-delete-session",
        "test",
        "interactive",
        "2026-07-15T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let _other_lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &db.node,
            &user_access,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            other_create,
        )
        .await
        .unwrap();
    let other = set_goal_from_access(
        &user_access,
        db.node_identity.did(),
        "other-delete-session",
        Some("keep this goal"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        delete_goals_for_session_from_access(
            &user_access,
            db.node_identity.did(),
            "user-delete-session",
        )
        .await
        .unwrap(),
        1
    );
    let rows = user_access
        .execute("{ Goal {_docID session_id} GoalCreationClaim {_docID session_id} }")
        .await
        .unwrap();
    let goals = rows["data"]["Goal"].as_array().unwrap();
    let claims = rows["data"]["GoalCreationClaim"].as_array().unwrap();
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0]["_docID"], other.doc_id);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0]["session_id"], "other-delete-session");
}

#[tokio::test]
async fn goal_and_claim_publication_links_match_lean() {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    use serde_json::json;

    async fn reader_sees(access: &ConfigAccess, query: &str, collection: &str) -> bool {
        let response = access.execute(query).await.unwrap();
        if let Some(errors) = response.get("errors") {
            assert!(
                errors.is_null() || errors.as_array().is_some_and(Vec::is_empty),
                "reader {collection} query failed: {response}"
            );
        }
        !response["data"][collection]
            .as_array()
            .unwrap_or_else(|| {
                panic!("reader {collection} query returned no rows array: {response}")
            })
            .is_empty()
    }

    for case in &crate::lean_vocab_test::lean_contract_snapshot().goal_link_cases {
        let (db, user, user_access, binding) =
            crate::test_support::signed_test_node("goal-links").await;
        let node = db.node.clone();
        let runtime = db.node_identity.clone();
        let agent =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let home = tempfile::tempdir().unwrap();
        let reader = KeyIdentity::load_or_create(home.path().join("reader"), None).unwrap();
        let admin =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
        for permission in [
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(agent.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        admin
            .grant_node_permission(
                &identity::Did::new(user.did()).unwrap(),
                NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
        for permission in [NodePermission::CollectionGet, NodePermission::DocumentRead] {
            admin
                .grant_node_permission(&identity::Did::new(reader.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let reader_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(reader.did()).unwrap());
        let agent_doc = crate::goal::client_control::agent_document_for_test(
            &user_access,
            runtime.did(),
            agent.did(),
        )
        .await;
        user_access
            .transact_write("test.goal_links.session", |txn| {
                let runtime = &runtime;
                let agent = &agent;
                let user = &user;
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        "links-session",
                        runtime.did(),
                        agent.did(),
                        Some(user.did()),
                        None,
                        None,
                        "2026-11-04T00:00:00Z",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let session_doc = user_access
            .transact_readonly("test.goal_links.session_document", |txn| {
                let runtime = &runtime;
                Box::pin(async move {
                    crate::session::load_agent_session_owner_in_txn(
                        txn,
                        runtime.did(),
                        "links-session",
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
            .doc_id;
        crate::session::grant_created_session_relationships(
            &user_access,
            &session_doc,
            &agent_doc,
            agent.did(),
        )
        .await
        .unwrap();
        user_access
            .add_actor_relationship(
                "AgentSession",
                &session_doc,
                "reader",
                &identity::Did::new(reader.did()).unwrap(),
            )
            .await
            .unwrap();
        let session_query = format!(
            r#"{{ AgentSession(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&session_doc)
        );
        assert!(
            reader_sees(&reader_access, &session_query, "AgentSession").await,
            "modeled reader must be able to query its granted session"
        );
        let (actor, access) = match case.actor.as_str() {
            "user" => (user.did(), &user_access),
            "agent" => (agent.did(), &agent_access),
            other => panic!("unknown model actor {other}"),
        };
        let input = json!({"goal_id": "model-goal", "creation_key": "model-claim", "node_did": runtime.did(),
            "session_id": "links-session", "session_doc_id": session_doc, "requester_did": user.did(),
            "agent_did": agent.did(), "creator_did": actor, "parent_linked": case.mode == "fresh",
            "objective": "Finish work", "created_at": "2026-11-04T00:00:00Z"});
        let author_access = if case.mode == "foreign" {
            &agent_access
        } else {
            access
        };
        let staged_doc = std::sync::Mutex::new(None::<String>);
        let outcome = author_access.transact_write("test.goal_links.publication", |txn| {
            let input = &input;
            let runtime = &runtime;
            let staged_doc = &staged_doc;
            Box::pin(async move {
                let doc = if case.mode == "fresh" {
                    let staged = stage_goal_and_claim(
                        txn, runtime.did(), "links-session", "Finish work", None,
                        GoalState { status: GoalStatus::Active, blocked_audits: 0,
                            wrapup_requested: false, wrapup_completed: false },
                        "2026-11-04T00:00:00Z", None, None,
                    ).await?;
                    if case.collection == "Goal" {
                        staged.goal.doc_id
                    } else {
                        let response = txn.execute("{ GoalCreationClaim { _docID } }").await?;
                        response["data"]["GoalCreationClaim"][0]["_docID"]
                            .as_str().unwrap().to_owned()
                    }
                } else {
                    let schema = match case.collection.as_str() {
                        "Goal" => gents_protocol::schemas::GOAL,
                        "GoalCreationClaim" => gents_protocol::schemas::GOAL_CREATION_CLAIM,
                        other => panic!("unknown modeled collection {other}"),
                    };
                    let mutation = format!(
                        "mutation($input: {}MutationInputArg!) {{ create_{}(input: $input) {{ _docID }} }}",
                        case.collection, case.collection,
                    );
                    let written = txn.execute_client_signed_create(
                        &mutation, &json!({"input": input}), schema,
                    ).await?;
                    crate::store::graphql::created_doc_id(&written, &case.collection)?
                };
                *staged_doc.lock().unwrap() = Some(doc.clone());
                if !case.commit {
                    anyhow::bail!("test requested publication rollback");
                }
                Ok::<_, anyhow::Error>(doc)
            })
        }).await;
        let doc = if case.commit {
            outcome.unwrap()
        } else {
            assert_eq!(
                outcome.unwrap_err().to_string(),
                "test requested publication rollback"
            );
            staged_doc.into_inner().unwrap().unwrap()
        };
        if case.mode == "foreign" {
            agent_access
                .add_actor_relationship(
                    &case.collection,
                    &doc,
                    "reader",
                    &identity::Did::new(actor).unwrap(),
                )
                .await
                .unwrap();
        }
        if case.mode == "partial" {
            access
                .add_resource_relationship(
                    &case.collection,
                    &doc,
                    "parent",
                    "sessions",
                    &session_doc,
                )
                .await
                .unwrap();
        }
        let query = format!(
            r#"{{ {}(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked}} }}"#,
            case.collection,
            escape_graphql_string(&doc)
        );
        let rows = access.execute(&query).await.unwrap();
        if case.owner.is_none() {
            assert!(rows["data"][&case.collection]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(!reader_sees(&reader_access, &query, &case.collection).await);
            assert!(case.steps.is_empty());
            node.shutdown().await.expect("node shutdown");
            continue;
        }
        if case.mode == "foreign" {
            assert_eq!(case.owner.as_deref(), Some("agent"));
            assert!(!case.repair_allowed);
            assert!(crate::goal::creation::finish_goal_relationships(
                access,
                runtime.did(),
                "links-session",
            )
            .await
            .is_err());
            let current = access.execute(&query).await.unwrap();
            assert_eq!(
                current["data"][&case.collection][0]["parent_linked"],
                case.initial.linked
            );
            assert!(!reader_sees(&reader_access, &query, &case.collection).await);
            node.shutdown().await.expect("node shutdown");
            continue;
        }
        assert_eq!(case.owner.as_deref(), Some(case.actor.as_str()));
        let owners = access
            .transact_readonly("test.goal_links.owner", |txn| {
                let collection = &case.collection;
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|did| did.as_str()),
            Some(actor)
        );
        assert_eq!(
            rows["data"][&case.collection][0]["parent_linked"],
            case.initial.linked
        );
        let initial_reader = reader_sees(&reader_access, &query, &case.collection).await;
        assert_eq!(initial_reader, case.initial.parent);
        if case.actor == "user" {
            let initial_agent = !agent_access.execute(&query).await.unwrap()["data"]
                [&case.collection]
                .as_array()
                .unwrap()
                .is_empty();
            assert_eq!(initial_agent, case.initial.worker || case.initial.parent);
        }
        for step in &case.steps {
            if step.parent_ok && step.worker_ok && step.mark_ok {
                crate::goal::creation::finish_goal_relationships(
                    access,
                    runtime.did(),
                    "links-session",
                )
                .await
                .unwrap();
            } else {
                let result: anyhow::Result<()> = access
                    .transact_write("test.goal_links.rollback", |attempt| {
                        let doc = &doc;
                        let session_doc = &session_doc;
                        let agent = &agent;
                        Box::pin(async move {
                            if step.parent_ok {
                                attempt
                                    .add_resource_relationship(
                                        &case.collection,
                                        doc,
                                        "parent",
                                        "sessions",
                                        session_doc,
                                    )
                                    .await?;
                            }
                            if step.worker_ok {
                                attempt
                                    .add_actor_relationship(
                                        &case.collection,
                                        doc,
                                        "working_agent",
                                        &identity::Did::new(agent.did())?,
                                    )
                                    .await?;
                            }
                            anyhow::bail!("test requested relationship rollback")
                        })
                    })
                    .await;
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "test requested relationship rollback"
                );
            }
            let current = access.execute(&query).await.unwrap();
            assert_eq!(
                current["data"][&case.collection][0]["parent_linked"],
                step.after.linked
            );
            let reader_visible = reader_sees(&reader_access, &query, &case.collection).await;
            assert_eq!(reader_visible, step.after.parent);
            if case.actor == "user" {
                let agent_visible = !agent_access.execute(&query).await.unwrap()["data"]
                    [&case.collection]
                    .as_array()
                    .unwrap()
                    .is_empty();
                assert_eq!(agent_visible, step.after.worker || step.after.parent);
            }
        }
        access
            .delete_resource_relationship(
                &case.collection,
                &doc,
                "parent",
                "sessions",
                &session_doc,
            )
            .await
            .unwrap();
        crate::goal::creation::finish_goal_relationships(access, runtime.did(), "links-session")
            .await
            .unwrap();
        let reader_visible = reader_sees(&reader_access, &query, &case.collection).await;
        assert_eq!(reader_visible, case.after_revocation.parent);
        let current = access.execute(&query).await.unwrap();
        assert_eq!(
            current["data"][&case.collection][0]["parent_linked"],
            case.after_revocation.linked
        );
        if case.actor == "user" {
            let agent_visible = !agent_access.execute(&query).await.unwrap()["data"]
                [&case.collection]
                .as_array()
                .unwrap()
                .is_empty();
            assert_eq!(agent_visible, case.after_revocation.worker);
        }
        node.shutdown().await.expect("node shutdown");
    }
}
