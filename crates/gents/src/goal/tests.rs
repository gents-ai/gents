use super::*;

#[tokio::test]
async fn session_token_usage_breakdown_sums_to_the_total() {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let owner = "did:key:z6MkTestOwner";
    let request = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "usage-request", purpose: "normal", node_did: "{owner}",
            session_id: "usage-session", agent_did: "general", content: "test",
            lifecycle_state: "completed", backend_id: "",
            created_at: "2026-07-15T00:00:00Z", retry_count: 0, max_retries: 0
        }}) {{ _docID }} }}"#
    );
    let response = node.execute(&request).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let request_doc_id =
        crate::store::graphql::single_mutation_document(&response, "create_AgentRequest")
            .unwrap()
            .and_then(|document| document.get("_docID"))
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_owned();
    for (seq, prompt, completion) in [(1, 1000, 50), (2, 200, 7)] {
        let call = format!(
            r#"mutation {{ add_InferenceCall(input: {{
                call_id: "usage-call-{seq}", runtime_instance_id: "goal-test",
                request_id: "usage-request", request_doc_id: "{request_doc_id}", call_seq: {seq}, backend_id: "backend-test",
                agent_did: "test", node_did: "{owner}", call_kind: "inference", attempt: 1,
                call_state: "completed", queued_at: "2026-07-15T00:00:00Z",
                started_at: "2026-07-15T00:00:00Z", ended_at: "2026-07-15T00:00:01Z",
                priority: 0, queue_depth_at_enqueue: 0, controller_generation: 1,
                backend_config_fingerprint: "goal-test",
                prompt_tokens: {prompt}, completion_tokens: {completion}
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&call).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let usage = crate::store::config_client::ConfigAccess::transact_local_readonly(
        &node,
        None,
        "goal.session_usage_breakdown_test",
        |txn| {
            Box::pin(async move {
                session_token_usage_breakdown_in_txn(txn, owner, "usage-session").await
            })
        },
    )
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
            &node,
            &identity::Did::new(owner).unwrap(),
            owner,
            "usage-session"
        )
        .await
        .unwrap(),
        1257
    );
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
    use crate::{ConfigAccess, KeyIdentity, NodeIdentity};

    let directory = tempfile::tempdir().unwrap();
    let runtime = KeyIdentity::load_or_create(directory.path().join("runtime.key"), None).unwrap();
    let agent = KeyIdentity::load_or_create(directory.path().join("agent.key"), None).unwrap();
    let node = std::sync::Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .build()
            .await
            .unwrap(),
    );
    let policy = node.add_dac_policy_as(
        &defra_core::RequestContext::ambient().with_identity(Some(agent.did().to_owned())),
        "name: Session goal owner\nresources:\n  - name: goals\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n"
    ).await.unwrap();
    node.add_collection(include_str!(
        "../../../gents-schemas/schemas/agent/agent_session.graphql"
    ))
    .await
    .unwrap();
    node.add_collection(&format!(
        "type Goal @policy(id: \"{}\", resource: \"goals\") {{ goal_id: String node_did: String session_id: String }}",
        escape_graphql_string(&policy)
    )).await.unwrap();
    let agent_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(agent.did()).unwrap());
    agent_access.write("test.selected_agent_session", &format!(
        "mutation {{ create_AgentSession(input: {{ session_id: \"session\", node_did: \"{}\", agent_did: \"{}\", created_at: \"2026-11-04T00:00:00Z\" }}) {{ _docID }} }}",
        escape_graphql_string(runtime.did()), escape_graphql_string(agent.did())
    )).await.unwrap();
    let created = agent_access.write("test.selected_agent_goal", &format!(
        "mutation {{ create_Goal(input: {{ goal_id: \"goal\", node_did: \"{}\", session_id: \"session\" }}) {{ _docID goal_id node_did session_id }} }}",
        escape_graphql_string(runtime.did())
    )).await.unwrap();
    let goal: GoalDocument =
        serde_json::from_value(created["data"]["create_Goal"][0].clone()).unwrap();
    node.add_dac_actor_relationship(agent.did(), "Goal", &goal.doc_id, "reader", runtime.did())
        .await
        .unwrap();
    let runtime_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let denied = runtime_access
        .write(
            "test.runtime_cannot_delete_goal",
            &format!(
                "mutation {{ delete_Goal(docID: \"{}\") {{ _docID }} }}",
                escape_graphql_string(&goal.doc_id)
            ),
        )
        .await;
    assert!(
        denied.is_err(),
        "runtime identity must not delete the selected agent's goal"
    );
    assert!(delete_goal(&node, &goal).await.unwrap());
    assert!(
        agent_access.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn goal_and_claim_publication_links_match_lean() {
    use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    use serde_json::json;
    use std::sync::Arc;

    for case in &crate::lean_vocab_test::lean_contract_snapshot().goal_link_cases {
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("node"), None).unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user"), None).unwrap();
        let agent = KeyIdentity::load_or_create(home.path().join("agent"), None).unwrap();
        let reader = KeyIdentity::load_or_create(home.path().join("reader"), None).unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let user_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(user.did()).unwrap());
        let agent_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(agent.did()).unwrap());
        let reader_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(reader.did()).unwrap());
        let (agent_doc, session_doc) = user_access.transact_write("test.goal_links.seed", |txn| Box::pin(async {
            let agent_row = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &json!({"input": {"node_did": runtime.did(), "agent_did": agent.did(), "creation_nonce": "a".repeat(64)}}),
            ).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&agent_row, "Agent")?;
            let session_row = txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &json!({"input": {"node_did": runtime.did(), "session_id": "links-session", "agent_did": agent.did(),
                    "requester_did": user.did(), "relationships_pending": true, "created_at": "2026-11-04T00:00:00Z"}}),
            ).await?;
            let session_doc = crate::store::graphql::created_doc_id(&session_row, "AgentSession")?;
            Ok::<_, anyhow::Error>((agent_doc, session_doc))
        })).await.unwrap();
        user_access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &identity::Did::new(agent.did()).unwrap(),
            )
            .await
            .unwrap();
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
        let (actor, access) = match case.actor.as_str() {
            "user" => (user.did(), &user_access),
            "agent" => (agent.did(), &agent_access),
            other => panic!("unknown model actor {other}"),
        };
        let input = json!({"goal_id": "model-goal", "creation_key": "model-claim", "node_did": runtime.did(),
            "session_id": "links-session", "session_doc_id": session_doc, "requester_did": user.did(),
            "agent_did": agent.did(), "creator_did": actor, "parent_linked": case.mode == "fresh",
            "objective": "Finish work", "created_at": "2026-11-04T00:00:00Z"});
        let author = if case.mode == "foreign" {
            agent.did()
        } else {
            actor
        };
        let txn = ConfigApplyTxn::begin_local(&node, Some(identity::Did::new(author).unwrap()))
            .await
            .unwrap();
        let mutation = format!(
            "mutation($input: {}MutationInputArg!) {{ create_{}(input: $input) {{ _docID }} }}",
            case.collection, case.collection
        );
        let doc = if case.mode == "fresh" {
            let staged = stage_goal_and_claim(
                &txn,
                runtime.did(),
                "links-session",
                "Finish work",
                None,
                GoalState {
                    status: GoalStatus::Active,
                    blocked_audits: 0,
                    wrapup_requested: false,
                    wrapup_completed: false,
                },
                "2026-11-04T00:00:00Z",
                None,
                None,
            )
            .await
            .unwrap();
            if case.collection == "Goal" {
                staged.goal.doc_id
            } else {
                let response = txn
                    .execute("{ GoalCreationClaim { _docID } }")
                    .await
                    .unwrap();
                response["data"]["GoalCreationClaim"][0]["_docID"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
        } else {
            let written = txn
                .execute_with_variables(&mutation, &json!({"input": input}))
                .await
                .unwrap();
            crate::store::graphql::created_doc_id(&written, &case.collection).unwrap()
        };
        if case.commit {
            txn.commit().await.unwrap();
        } else {
            txn.discard().await.unwrap();
        }
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
            assert!(
                reader_access.execute(&query).await.unwrap()["data"][&case.collection]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
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
            assert!(
                reader_access.execute(&query).await.unwrap()["data"][&case.collection]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
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
        let initial_reader = !reader_access.execute(&query).await.unwrap()["data"]
            [&case.collection]
            .as_array()
            .unwrap()
            .is_empty();
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
                let attempt =
                    ConfigApplyTxn::begin_local(&node, Some(identity::Did::new(actor).unwrap()))
                        .await
                        .unwrap();
                if step.parent_ok {
                    attempt
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
                if step.worker_ok {
                    attempt
                        .add_actor_relationship(
                            &case.collection,
                            &doc,
                            "working_agent",
                            &identity::Did::new(agent.did()).unwrap(),
                        )
                        .await
                        .unwrap();
                }
                attempt.discard().await.unwrap();
            }
            let current = access.execute(&query).await.unwrap();
            assert_eq!(
                current["data"][&case.collection][0]["parent_linked"],
                step.after.linked
            );
            let reader_visible = !reader_access.execute(&query).await.unwrap()["data"]
                [&case.collection]
                .as_array()
                .unwrap()
                .is_empty();
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
        let reader_visible = !reader_access.execute(&query).await.unwrap()["data"]
            [&case.collection]
            .as_array()
            .unwrap()
            .is_empty();
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
