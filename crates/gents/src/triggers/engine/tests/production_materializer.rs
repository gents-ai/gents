use super::*;

async fn materializer_with_node() -> (Arc<defra_node::EmbeddedNode>, ProductionMaterializer) {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_agent_and_schedules(integration_test_agent("general"), RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    (node, materializer)
}

#[tokio::test]
async fn task_opening_grants_same_node_interrupt_only_with_verified_fire() {
    let snapshot =
        snapshot_with_agent_and_schedules(integration_test_agent("general"), RapidHashMap::new());
    let owner = snapshot.local_did.clone();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(&owner)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: owner.clone(),
        trigger_id: "authority-trigger".into(),
        source_collection: "Schedule".into(),
        source_doc_id: "authority-occurrence".into(),
    };
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: gents_protocol::trigger_delivery::TriggerFire {
            fire_key: identity.fire_key(),
            request_id: identity.request_id(),
            session_id: identity.session_id(),
            identity,
            task_id: "authority-task".into(),
            goal_id: None,
            goal_objective: None,
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2030-01-01T00:00:00Z".into(),
        },
        target_existing: false,
        replace_prior: false,
    };
    let mut task = resolved_task("work");
    task.task_id = prepared.receipt.task_id.clone();
    materializer
        .materialize(
            &task,
            Some("authority-trigger"),
            TriggerKind::Schedule,
            Some("authority-trigger-document"),
            None,
            None,
            None,
            "work",
            None,
            &prepared.receipt.fire_key,
            Some(&prepared),
            None,
        )
        .await
        .unwrap();
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let rows = access
        .execute(&format!(
            "{{AgentRequest {{{}}}}}",
            crate::request::admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
    let request = crate::AgentRequest::try_from(row).unwrap();
    let scope = crate::session::origin::SessionScope::of_request(&request);
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &scope)
            .await
            .unwrap()
    );
    let session_request = request.clone();
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.task_session",
        move |txn| {
            let request = session_request.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &request.session_id,
                    &request.node_did,
                    &request.agent_id,
                    request.requester_did.as_deref(),
                    None,
                    crate::request::lifecycle::materialize::created_session_provenance(&request),
                    &request.created_at,
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    assert!(
        crate::session::origin::task_triggered_opening(&access, &scope)
            .await
            .unwrap()
    );
    let caller = crate::session::origin::SessionScope {
        session_id: "caller".into(),
        ..scope.clone()
    };
    assert!(
        crate::session::message::interrupt_refusal(&node, &caller, &scope)
            .await
            .unwrap()
            .is_none()
    );
    let mut caller_request = request.clone();
    caller_request.session_id = caller.session_id.clone();
    let tools = crate::tools::surface::AgentToolConfig {
        enabled: true,
        targets: vec![crate::documents::AgentTargetDocument {
            target_id: "worker".into(),
            node_did: owner.clone(),
            target_node_did: owner.clone(),
            agent_id: request.agent_id.clone(),
            name: "worker".into(),
            description: None,
            tags: Vec::new(),
        }],
    };
    let listed = crate::session::message::agent_session_list(
        &node,
        &caller_request,
        &tools,
        &crate::session::message::AgentSessionListArgs::default(),
    )
    .await
    .unwrap();
    let entry = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == scope.session_id)
        .unwrap();
    assert_eq!(entry["can_interrupt"], true);
    let wrong_requester = crate::session::origin::SessionScope {
        requester_did: Some("did:wrong:requester".into()),
        ..scope.clone()
    };
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &wrong_requester)
            .await
            .unwrap()
    );
    let foreign = crate::session::origin::SessionScope {
        node_did: "did:other".into(),
        ..caller
    };
    assert!(
        crate::session::message::interrupt_refusal(&node, &foreign, &scope)
            .await
            .unwrap()
            .is_some()
    );
    access.write("test.ambiguous_fire", &format!(
        r#"mutation {{create_TriggerFire(input: {{fire_key: "duplicate-fire", owner_did: "{}", request_id: "{}", session_id: "{}", trigger_id: "authority-trigger", task_id: "authority-task"}}) {{_docID}}}}"#,
        escape_graphql_string(&owner), escape_graphql_string(&request.request_id), escape_graphql_string(&scope.session_id)
    )).await.unwrap();
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &scope)
            .await
            .unwrap()
    );
    let listed = crate::session::message::agent_session_list(
        &node,
        &caller_request,
        &tools,
        &crate::session::message::AgentSessionListArgs::default(),
    )
    .await
    .unwrap();
    let entry = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == scope.session_id)
        .unwrap();
    assert_eq!(entry["can_interrupt"], false);

    access.write("test.remove_fires", &format!(
        r#"mutation {{delete_TriggerFire(filter: {{owner_did: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
        escape_graphql_string(&owner), escape_graphql_string(&request.request_id)
    )).await.unwrap();
    access.write("test.wrong_trigger_fire", &format!(
        r#"mutation {{create_TriggerFire(input: {{fire_key: "wrong-trigger-fire", owner_did: "{}", request_id: "{}", session_id: "{}", trigger_id: "other-trigger", task_id: "authority-task"}}) {{_docID}}}}"#,
        escape_graphql_string(&owner), escape_graphql_string(&request.request_id), escape_graphql_string(&scope.session_id)
    )).await.unwrap();
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &scope)
            .await
            .unwrap()
    );
    access.write("test.remove_wrong_fire",
        r#"mutation {delete_TriggerFire(filter: {fire_key: {_eq: "wrong-trigger-fire"}}) {_docID}}"#
    ).await.unwrap();
    access.write("test.replace_fire_task", &format!(
        r#"mutation {{create_TriggerFire(input: {{fire_key: "{}", owner_did: "{}", request_id: "{}", session_id: "{}", trigger_id: "authority-trigger", task_id: "different-task", source_collection: "Schedule", source_doc_id: "authority-occurrence"}}) {{_docID}}}}"#,
        escape_graphql_string(&prepared.receipt.fire_key), escape_graphql_string(&owner), escape_graphql_string(&request.request_id), escape_graphql_string(&scope.session_id)
    )).await.unwrap();
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &scope)
            .await
            .unwrap()
    );
    access.write("test.remove_task_session", &format!(
        r#"mutation {{delete_AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
        escape_graphql_string(&owner), escape_graphql_string(&scope.session_id)
    )).await.unwrap();
    let original_request = request.clone();
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.wrong_requester_session",
        move |txn| {
            let request = original_request.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &request.session_id,
                    &request.node_did,
                    &request.agent_id,
                    Some("did:wrong:requester"),
                    None,
                    crate::request::lifecycle::materialize::created_session_provenance(&request),
                    &request.created_at,
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let wrong_requester_scope = crate::session::origin::SessionScope {
        requester_did: Some("did:wrong:requester".into()),
        ..scope.clone()
    };
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &wrong_requester_scope)
            .await
            .unwrap()
    );

    let mut forged = rows["data"]["AgentRequest"][0].clone();
    let fields = forged.as_object_mut().unwrap();
    fields.remove("_docID");
    fields.remove("input");
    fields.retain(|_, value| !value.is_null());
    fields.insert(
        "request_id".into(),
        serde_json::json!("forged-authority-request"),
    );
    fields.insert(
        "session_id".into(),
        serde_json::json!("forged-authority-session"),
    );
    fields.insert(
        "admission_signature".into(),
        serde_json::json!("invalid-signature"),
    );
    fields.insert(
        "retry_key".into(),
        serde_json::json!("forged-authority-retry"),
    );
    let input = gents_protocol::graphql::graphql_input_literal(&forged).unwrap();
    access
        .write(
            "test.forged_opening",
            &format!("mutation {{create_AgentRequest(input: {input}) {{_docID}}}}"),
        )
        .await
        .unwrap();
    let forged_rows = access
        .execute(&format!(
        "{{AgentRequest(filter: {{request_id: {{_eq: \"forged-authority-request\"}}}}) {{{}}}}}",
        crate::request::admission::SIGNED_REQUEST_FIELDS,
    ))
        .await
        .unwrap();
    let forged_row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(forged_rows["data"]["AgentRequest"][0].clone()).unwrap();
    assert!(crate::request::admission::verify_request_receipt_signature(&forged_row).is_err());
    let forged_request = crate::AgentRequest::try_from(forged_row).unwrap();
    let forged_scope = crate::session::origin::SessionScope::of_request(&forged_request);
    access.write("test.forged_fire", &format!(
        r#"mutation {{create_TriggerFire(input: {{fire_key: "forged-fire", owner_did: "{}", request_id: "forged-authority-request", session_id: "forged-authority-session", trigger_id: "authority-trigger", task_id: "authority-task"}}) {{_docID}}}}"#,
        escape_graphql_string(&owner)
    )).await.unwrap();
    let missing_doc = document::DocID::new_v0_from_seed("missing-task-opening").to_string();
    for (test_session, opening, requester) in [
        (
            "forged-authority-session",
            forged_request.doc_id.clone(),
            forged_request.requester_did.clone(),
        ),
        (
            "missing-authority-session",
            missing_doc,
            forged_request.requester_did.clone(),
        ),
        (
            "wrong-requester-session",
            forged_request.doc_id.clone(),
            Some("did:wrong:requester".into()),
        ),
    ] {
        let session = test_session.to_owned();
        let node_did = owner.clone();
        crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "test.bad_opening_session",
            move |txn| {
                let session = session.clone();
                let node_did = node_did.clone();
                let opening = opening.clone();
                let requester = requester.clone();
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        &session,
                        &node_did,
                        "general",
                        requester.as_deref(),
                        None,
                        Some(gents_protocol::session::SessionProvenance {
                            opening_request_doc_id: Some(opening),
                            ..Default::default()
                        }),
                        "2030-01-01T00:00:00Z",
                    )
                    .await
                })
            },
        )
        .await
        .unwrap();
        let bad_scope = crate::session::origin::SessionScope {
            node_did: owner.clone(),
            session_id: test_session.into(),
            requester_did: if test_session == "wrong-requester-session" {
                Some("did:wrong:requester".into())
            } else {
                forged_scope.requester_did.clone()
            },
        };
        assert!(
            !crate::session::origin::task_triggered_opening(&access, &bad_scope)
                .await
                .unwrap(),
            "{test_session}"
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn goal_task_materialization_is_atomic_and_idempotent_for_one_durable_fire() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    let node_did = behavior.node_did().to_string();
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "task-goal-release".to_string(),
        name: Some("release task".to_string()),
        agent_id: "general".to_string(),
        prompt_template: "implement release".to_string(),
        goal_objective_template: Some("ship release".to_string()),
        goal_token_budget: Some(4_096),
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let fire_key = "event:release-trigger:doc:release-42";
    let identity = crate::goal::task_goal_fire_identity(&node_did, &task.task_id, fire_key);

    let invalid = materializer
        .materialize(
            &task,
            Some("release-trigger"),
            TriggerKind::Event,
            Some("release-trigger-doc"),
            Some("release-42"),
            Some("batch-7"),
            Some(r#"{"version":1,"source_fields":{"requester_did":"did:key:z-requester"}}"#),
            "implement release",
            Some("   "),
            fire_key,
            None,
            None,
        )
        .await
        .expect_err("an invalid declaration must roll back before publication");
    assert!(invalid.to_string().contains("non-empty"), "{invalid:#}");
    assert!(
        crate::goal::load_canonical_goal(node.as_ref(), &node_did, &identity.session_id)
            .await
            .unwrap()
            .is_none(),
        "failed materialization must not leave a Goal"
    );

    let (first, mutation_count) =
        crate::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
            || async {
                materializer
                    .materialize(
                        &task,
                        Some("release-trigger"),
                        TriggerKind::Event,
                        Some("release-trigger-doc"),
                        Some("release-42"),
                        Some("batch-7"),
                        Some(r#"{"version":1,"source_fields":{"requester_did":"did:key:z-requester"}}"#),
                        "implement release",
                        Some("ship release"),
                        fire_key,
                        None,
                        None,
                    )
                    .await
            },
            || async {
                crate::config_client::ConfigAccess::local(node.clone())
                    .execute(
                        "{ Goal {_docID assignment_root_request_doc_id} GoalCreationClaim {_docID} AgentRequest {_docID input} }",
                    )
                    .await
                    .unwrap()
            },
        )
        .await;
    assert!(
        mutation_count >= 4,
        "Goal, creation claim, signed request, and physical root share one commit"
    );
    let retry = materializer
        .materialize(
            &task,
            Some("release-trigger"),
            TriggerKind::Event,
            Some("release-trigger-doc"),
            Some("release-42"),
            Some("batch-7"),
            Some(r#"{"version":1,"source_fields":{"requester_did":"did:key:z-requester"}}"#),
            "implement release",
            Some("ship release"),
            fire_key,
            None,
            None,
        )
        .await
        .expect("exact fire retry");
    assert_eq!(first, identity.request_id);
    assert_eq!(retry, identity.request_id);
    assert_eq!(
        materializer
            .recover_goal_task_fire(&task, fire_key)
            .await
            .unwrap(),
        Some(identity.request_id.clone())
    );

    let goal = crate::goal::load_canonical_goal(node.as_ref(), &node_did, &identity.session_id)
        .await
        .unwrap()
        .expect("Goal must commit with its first request");
    assert_eq!(goal.objective, "ship release");
    assert_eq!(goal.token_budget, Some(4_096));
    assert_eq!(goal.status, "active");

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
                _docID request_id node_did session_id retry_key lifecycle_state content input
                caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind
                caused_by_source_doc_id caused_by_correlation admission_kind
                runtime_source_request_id runtime_source_kind
            }}
            GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}) {{
                node_did session_id objective token_budget
            }}
        }}"#,
        escape_graphql_string(&identity.retry_key),
        escape_graphql_string(&crate::goal::deterministic_goal_creation_key(
            &node_did,
            &identity.session_id,
        )),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentRequest rows");
    let claims = response
        .data
        .as_ref()
        .and_then(|data| data.get("GoalCreationClaim"))
        .and_then(serde_json::Value::as_array)
        .expect("GoalCreationClaim rows");
    assert_eq!(requests.len(), 1, "retry must not duplicate requests");
    assert_eq!(claims.len(), 1, "retry must not duplicate creation claims");
    let request = &requests[0];
    assert_eq!(
        goal.assignment_root_request_doc_id.as_deref(),
        request["_docID"].as_str()
    );
    assert_eq!(request["input"]["goal_assignment"]["goal_id"], goal.goal_id);
    assert!(request["input"]["goal_assignment"]["root_request_doc_id"].is_null());
    assert_eq!(
        request["request_id"].as_str(),
        Some(identity.request_id.as_str())
    );
    assert_eq!(request["node_did"].as_str(), Some(node_did.as_str()));
    assert_eq!(
        request["session_id"].as_str(),
        Some(identity.session_id.as_str())
    );
    assert_eq!(
        request["retry_key"].as_str(),
        Some(identity.retry_key.as_str())
    );
    assert_eq!(request["lifecycle_state"].as_str(), Some("pending"));
    assert_eq!(request["content"].as_str(), Some("implement release"));
    assert_eq!(
        request["caused_by_trigger_id"].as_str(),
        Some("release-trigger")
    );
    assert_eq!(
        request["caused_by_trigger_doc_id"].as_str(),
        Some("release-trigger-doc")
    );
    assert_eq!(request["caused_by_trigger_kind"].as_str(), Some("event"));
    assert_eq!(
        request["caused_by_source_doc_id"].as_str(),
        Some("release-42")
    );
    assert_eq!(request["caused_by_correlation"].as_str(), Some("batch-7"));
    assert_eq!(request["admission_kind"].as_str(), Some("runtime-internal"));
    assert_eq!(
        request["runtime_source_request_id"].as_str(),
        Some("release-trigger")
    );
    assert_eq!(
        request["runtime_source_kind"].as_str(),
        Some("automated-trigger")
    );

    assert_eq!(
        crate::goal::delete_goals_for_session(&node, &node_did, &identity.session_id)
            .await
            .expect("clear Goal and creation claim before source checkpoint"),
        1
    );
    assert_eq!(
        materializer
            .recover_goal_task_fire(&task, fire_key)
            .await
            .expect("the exact persisted request is independently checkpointable"),
        Some(identity.request_id),
        "clearing terminal controller state must not replay the Task fire"
    );
}

#[tokio::test]
async fn durable_goal_task_replay_retains_its_admitted_assignment() {
    let snapshot =
        snapshot_with_agent_and_schedules(integration_test_agent("general"), RapidHashMap::new());
    let owner = snapshot.local_did.clone();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(&owner)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let session = "replaced-task-goal-session";
    crate::session::ensure_session_with_agent_id_and_requester_did(
        &node,
        session,
        &owner,
        "general",
        Some(&owner),
    )
    .await
    .unwrap();
    let initial = crate::goal::deterministic_goal_id(&owner, session);
    let selected = crate::goal::replacement_goal_identity(&owner, session, "first-replacement");
    let access = crate::config_client::ConfigAccess::local(node.clone());
    access
        .write(
            "test.replay_current_goal",
            &format!(
                r#"mutation {{create_Goal(input: {{
        node_did: "{}", session_id: "{}", goal_id: "{}", creation_key: "{}",
        objective: "Finish the task", status: "active", created_at: "2030-01-01T00:00:00Z"
    }}) {{_docID}}}}"#,
                escape_graphql_string(&owner),
                escape_graphql_string(session),
                escape_graphql_string(&selected.goal_id),
                escape_graphql_string(&selected.creation_key)
            ),
        )
        .await
        .unwrap();
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: owner.clone(),
        trigger_id: "replay-goal-trigger".into(),
        source_collection: "Schedule".into(),
        source_doc_id: "original-due-occurrence".into(),
    };
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: gents_protocol::trigger_delivery::TriggerFire {
            fire_key: identity.fire_key(),
            request_id: identity.request_id(),
            identity,
            task_id: "replay-goal-task".into(),
            session_id: session.into(),
            goal_id: Some(initial),
            goal_objective: Some("Finish the task".into()),
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2030-01-01T00:00:00Z".into(),
        },
        target_existing: true,
        replace_prior: false,
    };
    let mut task = resolved_task("Finish the task");
    task.task_id = prepared.receipt.task_id.clone();
    task.goal_objective_template = prepared.receipt.goal_objective.clone();
    let publish = || {
        materializer.materialize(
            &task,
            Some("replay-goal-trigger"),
            TriggerKind::Schedule,
            Some("replay-goal-trigger-document"),
            None,
            None,
            None,
            "Finish the task",
            Some("Finish the task"),
            &prepared.receipt.fire_key,
            Some(&prepared),
            None,
        )
    };
    assert_eq!(publish().await.unwrap(), prepared.receipt.request_id);
    let query = format!(
        "{{AgentRequest {{{}}} TriggerFire {{_docID goal_id request_id}}}}",
        crate::request::admission::SIGNED_REQUEST_FIELDS
    );
    let admitted = access.execute(&query).await.unwrap();
    let existing_scope = crate::session::origin::SessionScope {
        node_did: owner.clone(),
        session_id: session.to_owned(),
        requester_did: Some(owner.clone()),
    };
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &existing_scope)
            .await
            .unwrap()
    );
    let later_doc = admitted["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    access.write("test.later_task_is_not_creation", &format!(
        r#"mutation {{update_AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, input: {{provenance: {{opening_request_doc_id: "{}"}}}}) {{_docID}}}}"#,
        escape_graphql_string(&owner), escape_graphql_string(session), escape_graphql_string(later_doc),
    )).await.unwrap();
    assert!(
        !crate::session::origin::task_triggered_opening(&access, &existing_scope)
            .await
            .unwrap()
    );
    assert_eq!(
        admitted["data"]["TriggerFire"][0]["goal_id"],
        selected.goal_id
    );
    assert_eq!(
        admitted["data"]["AgentRequest"][0]["input"]["goal_assignment"]["goal_id"],
        selected.goal_id
    );
    assert!(
        admitted["data"]["AgentRequest"][0]["input"]["goal_assignment"]["root_request_doc_id"]
            .is_null()
    );
    let goal = crate::goal::load_canonical_goal(&node, &owner, session)
        .await
        .unwrap()
        .unwrap();
    let replacement = crate::goal::replacement_goal_identity(&owner, session, "later-replacement");
    access
        .write(
            "test.replay_later_goal",
            &format!(
                "mutation {{update_Goal(docID: \"{}\", input: {{goal_id: \"{}\"}}) {{_docID}}}}",
                escape_graphql_string(&goal.doc_id),
                escape_graphql_string(&replacement.goal_id),
            ),
        )
        .await
        .unwrap();
    let duplicate = publish().await.unwrap_err();
    assert_eq!(
        duplicate
            .downcast_ref::<MaterializeDuplicate>()
            .unwrap()
            .request_id,
        prepared.receipt.request_id
    );
    assert_eq!(access.execute(&query).await.unwrap(), admitted);
    assert_eq!(
        crate::goal::load_canonical_goal(&node, &owner, session)
            .await
            .unwrap()
            .unwrap()
            .goal_id,
        replacement.goal_id
    );
    node.shutdown().await;
}

#[tokio::test]
async fn goal_task_identity_and_recovery_are_scoped_by_node_did() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let first_behavior = integration_test_agent("general");
    let first_did = first_behavior.node_did().to_string();
    let first_snapshot = snapshot_with_agent_and_schedules(first_behavior, RapidHashMap::new());
    let (_first_tx, first_rx) = watch::channel(first_snapshot);
    let first_materializer = ProductionMaterializer::new(node.clone(), first_rx);
    let second_behavior = integration_test_agent("general");
    let second_did = second_behavior.node_did().to_string();
    let second_snapshot = snapshot_with_agent_and_schedules(second_behavior, RapidHashMap::new());
    let (_second_tx, second_rx) = watch::channel(second_snapshot);
    let second_materializer = ProductionMaterializer::new(node, second_rx);
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "shared-task-id".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "shared prompt".to_string(),
        goal_objective_template: Some("shared objective".to_string()),
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let fire_key = "shared-fire-key";

    let first_request = first_materializer
        .materialize(
            &task,
            Some("shared-trigger"),
            TriggerKind::Schedule,
            Some("shared-trigger-doc"),
            None,
            None,
            None,
            "shared prompt",
            Some("shared objective"),
            fire_key,
            None,
            None,
        )
        .await
        .expect("first DID materialization");
    let second_request = second_materializer
        .materialize(
            &task,
            Some("shared-trigger"),
            TriggerKind::Schedule,
            Some("shared-trigger-doc"),
            None,
            None,
            None,
            "shared prompt",
            Some("shared objective"),
            fire_key,
            None,
            None,
        )
        .await
        .expect("second DID materialization");

    assert_ne!(first_did, second_did);
    assert_ne!(first_request, second_request);
    assert_eq!(
        first_materializer
            .recover_goal_task_fire(&task, fire_key)
            .await
            .expect("first DID recovery"),
        Some(first_request)
    );
    assert_eq!(
        second_materializer
            .recover_goal_task_fire(&task, fire_key)
            .await
            .expect("second DID recovery"),
        Some(second_request)
    );
}

#[tokio::test]
async fn goal_task_recovery_rejects_foreign_principal_using_expected_request_id() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    let node_did = behavior.node_did().to_string();
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "foreign-collision-task".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "prompt".to_string(),
        goal_objective_template: Some("objective".to_string()),
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let fire_key = "foreign-collision-fire";
    let identity = crate::goal::task_goal_fire_identity(&node_did, &task.task_id, fire_key);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{}",
                purpose: "normal",
                node_did: "did:key:foreign-task-owner",
                agent_id: "general",
                session_id: "{}",
                retry_key: "{}",
                content: "foreign collision",
                lifecycle_state: "pending"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(&identity.request_id),
        escape_graphql_string(&identity.session_id),
        escape_graphql_string(&identity.retry_key),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed collision: {:?}",
        response.errors
    );

    let error = materializer
        .recover_goal_task_fire(&task, fire_key)
        .await
        .expect_err("wrong-principal deterministic binding must conflict");
    assert!(
        error.to_string().contains("identity conflicts"),
        "{error:#}"
    );
}

async fn create_request(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
    node_did: &str,
    lifecycle_state: &str,
    trigger_id: &str,
    trigger_kind: TriggerKind,
    correlation: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let node_did = escape_graphql_string(node_did);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let trigger_id = escape_graphql_string(trigger_id);
    let correlation = escape_graphql_string(correlation);
    let created_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                purpose: "normal",
                node_did: "{node_did}",
                agent_id: "general",
                session_id: "{request_id}",
                content: "production marker test",
                lifecycle_state: "{lifecycle_state}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                backend_id: "",
                execution_origin: "automated-trigger",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3,
                caused_by_trigger_id: "{trigger_id}",
                caused_by_trigger_kind: "{trigger_kind}",
                caused_by_correlation: "{correlation}"
            }}) {{ _docID }}
        }}"#,
        trigger_kind = trigger_kind.as_str(),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "creating AgentRequest {request_id} failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn durable_group_marker_preserves_generation_owner_and_goal_mode() {
    let (node, materializer) = materializer_with_node().await;
    let owner = "did:key:z-marker-owner";
    let trigger = "review-\"verify";
    let correlation = "run-\"42";
    let old = crate::triggers::engine::durable_fire_key("event-group", &["old-membership"]);
    let next = crate::triggers::engine::durable_fire_key("event-group", &["new-membership"]);
    create_request(
        &node,
        &old,
        owner,
        "completed",
        trigger,
        TriggerKind::Event,
        correlation,
    )
    .await;
    assert!(materializer
        .has_materialized_group_request(owner, trigger, &old)
        .await
        .unwrap());
    assert!(
        !materializer
            .has_materialized_group_request(owner, trigger, &next)
            .await
            .unwrap(),
        "same correlation in a different generation must remain eligible"
    );
    assert!(!materializer
        .has_materialized_group_request("did:key:z-other", trigger, &old)
        .await
        .unwrap());
    assert!(!materializer
        .has_materialized_group_request(owner, "other-trigger", &old)
        .await
        .unwrap());
    let goal = crate::goal::task_goal_fire_identity(owner, "old-task-name", &next);
    create_request(
        &node,
        &goal.request_id,
        owner,
        "completed",
        trigger,
        TriggerKind::Event,
        correlation,
    )
    .await;
    assert!(
        materializer
            .has_materialized_group_request(owner, trigger, &next)
            .await
            .unwrap(),
        "goal task still marks the same delivery after task rename or goal-mode switch"
    );
    let later = crate::triggers::engine::durable_fire_key("event-group", &["third-membership"]);
    assert!(!materializer
        .has_materialized_group_request(owner, trigger, &later)
        .await
        .unwrap());
}

#[tokio::test]
async fn materializer_rejects_workspace_from_different_explicit_owner() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let behavior = integration_test_agent("general");
    insert_ready_workspace(
        &node,
        "ws-owner",
        "did:key:z-correct-owner",
        behavior.node_did(),
    )
    .await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let context = writer_context("ws-owner", "did:key:z-wrong-owner");
    let error = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-owner"),
            TriggerKind::Event,
            Some("trigger-owner-doc"),
            Some("source"),
            Some("corr"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error:#}");
    assert!(
        workspace_requests(&node, "ws-owner").await.is_empty(),
        "wrong owner must reject before request publication"
    );
}

#[tokio::test]
async fn trigger_wide_gate_and_supersede_survive_kind_change_and_preserve_exclusion() {
    let (node, materializer) = materializer_with_node().await;
    let owner = "did:key:z-concurrency-owner";
    let trigger = "review-verify";
    create_request(
        &node,
        "old-schedule",
        owner,
        "pending",
        trigger,
        TriggerKind::Schedule,
        "old-correlation",
    )
    .await;
    assert!(
        materializer
            .has_active_runtime_request_for_trigger(owner, trigger, None)
            .await
            .unwrap(),
        "changing Trigger.source to Event cannot forget a prior Schedule request"
    );
    create_request(
        &node,
        "new-event",
        owner,
        "pending",
        trigger,
        TriggerKind::Event,
        "new-correlation",
    )
    .await;
    create_request(
        &node,
        "terminal",
        owner,
        "completed",
        trigger,
        TriggerKind::Event,
        "terminal-correlation",
    )
    .await;
    create_request(
        &node,
        "foreign-owner",
        "other-owner",
        "pending",
        trigger,
        TriggerKind::Event,
        "new-correlation",
    )
    .await;
    create_request(
        &node,
        "foreign-trigger",
        owner,
        "pending",
        "other-trigger",
        TriggerKind::Event,
        "new-correlation",
    )
    .await;
    assert!(materializer
        .has_active_runtime_request_for_trigger(owner, trigger, Some("old-schedule"))
        .await
        .unwrap());
    assert_eq!(
        materializer
            .supersede_active_runtime_requests_for_trigger(owner, trigger, Some("old-schedule"))
            .await
            .unwrap(),
        1
    );
    assert!(
        !materializer
            .has_active_runtime_request_for_trigger(owner, trigger, Some("old-schedule"))
            .await
            .unwrap(),
        "self-retry exclusion must retain its own request and remove only the competing request"
    );
    assert!(materializer
        .has_active_runtime_request_for_trigger(owner, trigger, None)
        .await
        .unwrap());
    assert_eq!(
        materializer
            .supersede_active_runtime_requests_for_trigger(owner, trigger, None)
            .await
            .unwrap(),
        1
    );
    assert!(!materializer
        .has_active_runtime_request_for_trigger(owner, trigger, None)
        .await
        .unwrap());
    assert!(materializer
        .has_active_runtime_request_for_trigger("other-owner", trigger, None)
        .await
        .unwrap());
    assert!(materializer
        .has_active_runtime_request_for_trigger(owner, "other-trigger", None)
        .await
        .unwrap());
}

fn workspace_writer_task() -> ResolvedTask {
    ResolvedTask {
        emit_outcome: false,
        task_id: "task-ws".to_string(),
        name: None,
        agent_id: "general".to_string(),
        prompt_template: "patch".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    }
}

fn writer_context(workspace_id: &str, owner: &str) -> String {
    serde_json::json!({"version":1,"source_fields":{"workspace_id":workspace_id,"workspace_owner_node_did":owner,"workspace_authority":"readWrite"}}).to_string()
}

async fn insert_ready_workspace(
    node: &defra_node::EmbeddedNode,
    workspace_id: &str,
    owner: &str,
    writer_principal: &str,
) {
    let mutation = crate::tools::workspace::isolated_workspace_upsert_mutation(
        &crate::tools::workspace::IsolatedWorkspaceDoc {
            path_capability: crate::tools::workspace::WorkspacePathCapability::exact_paths(vec![])
                .unwrap(),
            workspace_id: workspace_id.into(),
            work_unit_id: "unit-1".into(),
            repository_id: "repo-1".into(),
            base_sha: "abc".into(),
            branch: "topic".into(),
            creation_policy: "git_worktree_diff".into(),
            adapter: "git_worktree".into(),
            owner_node_did: owner.into(),
            writer_principal: writer_principal.into(),
            integrator_principal: "did:key:integrator".into(),
            instruction_manifest: "{}".into(),
            seal_hash: None,
            lifecycle_state: "ready".into(),
            caused_by_invocation_id: "inv-1".into(),
            caused_by_correlation: "corr-1".into(),
        },
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "creating IsolatedWorkspace failed: {:?}",
        response.errors
    );
}

async fn workspace_requests(
    node: &defra_node::EmbeddedNode,
    workspace_id: &str,
) -> Vec<serde_json::Value> {
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ workspace_id: {{ _eq: "{id}" }} }}) {{
                request_id
                node_did
                lifecycle_state
                workspace_id
                workspace_seal_hash
                workspace_owner_node_did
                workspace_authority
            }}
        }}"#,
        id = escape_graphql_string(workspace_id),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "querying workspace requests failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn materializer_resolves_missing_workspace_owner_from_canonical_workspace() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let behavior = integration_test_agent("general");
    let owner = behavior.node_did().to_string();
    insert_ready_workspace(&node, "ws-incomplete", &owner, &owner).await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let context=serde_json::json!({"version":1,"source_fields":{"workspace_id":"ws-incomplete","workspace_authority":"readWrite"}}).to_string();
    let request_id = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-incomplete"),
            TriggerKind::Event,
            Some("trigger-incomplete-doc"),
            Some("source"),
            Some("corr"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
            None,
            None,
        )
        .await
        .expect("canonical workspace resolves the missing owner");
    let rows = workspace_requests(&node, "ws-incomplete").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["request_id"].as_str(), Some(request_id.as_str()));
    assert_eq!(
        rows[0]["workspace_owner_node_did"].as_str(),
        Some(owner.as_str())
    );
    assert_eq!(rows[0]["workspace_authority"].as_str(), Some("readWrite"));
}

#[tokio::test]
async fn materializer_does_not_infer_an_owner_for_a_foreign_workspace() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let behavior = integration_test_agent("general");
    let owner = behavior.node_did().to_string();
    insert_ready_workspace(
        &node,
        "ws-foreign-ownerless",
        "did:key:another-owner",
        &owner,
    )
    .await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let context = serde_json::json!({"version":1,"source_fields":{"workspace_id":"ws-foreign-ownerless","workspace_authority":"readOnly"}}).to_string();
    let error = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-foreign-ownerless"),
            TriggerKind::Event,
            Some("trigger-foreign-ownerless-doc"),
            Some("source"),
            Some("corr"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("isolated workspace ws-foreign-ownerless not found"),
        "{error:#}"
    );
    assert!(workspace_requests(&node, "ws-foreign-ownerless")
        .await
        .is_empty());
}

#[tokio::test]
async fn materializer_preserves_explicit_workspace_owner_distinct_from_executor() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    let executor = behavior.node_did().to_owned();
    assert_ne!(executor, "did:key:z-workspace-owner");
    insert_ready_workspace(
        node.as_ref(),
        "ws-stamp",
        "did:key:z-workspace-owner",
        behavior.node_did(),
    )
    .await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let context = writer_context("ws-stamp", "did:key:z-workspace-owner");
    let request_id = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-stamp"),
            TriggerKind::Event,
            Some("trigger-stamp-config-doc"),
            Some("src-stamp"),
            Some("corr-stamp"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
            None,
            None,
        )
        .await
        .expect("explicit workspace owner survives request publication");
    let rows = workspace_requests(node.as_ref(), "ws-stamp").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["node_did"].as_str(), Some(executor.as_str()));
    assert_eq!(rows[0]["workspace_id"].as_str(), Some("ws-stamp"));
    assert_eq!(rows[0]["workspace_authority"].as_str(), Some("readWrite"));
    assert_eq!(rows[0]["request_id"].as_str(), Some(request_id.as_str()));
    assert_eq!(
        rows[0]["workspace_owner_node_did"].as_str(),
        Some("did:key:z-workspace-owner")
    );
}

#[tokio::test]
async fn goal_task_workspace_activation_retry_is_idempotent() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    insert_ready_workspace(
        node.as_ref(),
        "ws-goal-retry",
        "did:key:z-workspace-owner",
        behavior.node_did(),
    )
    .await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let context = writer_context("ws-goal-retry", "did:key:z-workspace-owner");
    let mut task = workspace_writer_task();
    task.goal_objective_template = Some("finish workspace change".to_string());
    task.goal_token_budget = Some(2_048);
    let fire_key = "event:trigger-workspace-goal:doc:source-workspace-goal";

    let first = materializer
        .materialize(
            &task,
            Some("trigger-workspace-goal"),
            TriggerKind::Event,
            Some("trigger-workspace-goal-doc"),
            Some("source-workspace-goal"),
            Some("corr-workspace-goal"),
            Some(&context),
            "prompt",
            Some("finish workspace change"),
            fire_key,
            None,
            None,
        )
        .await
        .expect("first goal-backed workspace fire");
    let retry = materializer
        .materialize(
            &task,
            Some("trigger-workspace-goal"),
            TriggerKind::Event,
            Some("trigger-workspace-goal-doc"),
            Some("source-workspace-goal"),
            Some("corr-workspace-goal"),
            Some(&context),
            "prompt",
            Some("finish workspace change"),
            fire_key,
            None,
            None,
        )
        .await
        .expect("activation acknowledgement retry");

    assert_eq!(retry, first);
    let rows = workspace_requests(node.as_ref(), "ws-goal-retry").await;
    assert_eq!(rows.len(), 1, "retry must reuse the staged request");
    assert_eq!(
        rows[0]["workspace_owner_node_did"].as_str(),
        Some("did:key:z-workspace-owner")
    );
    assert_eq!(rows[0]["lifecycle_state"].as_str(), Some("pending"));
}

#[tokio::test]
async fn unique_read_write_denial_does_not_leave_claimable_request() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    insert_ready_workspace(
        node.as_ref(),
        "ws-rw",
        "did:key:z-workspace-owner",
        behavior.node_did(),
    )
    .await;
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let context = writer_context("ws-rw", "did:key:z-workspace-owner");
    let first = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-rw-1"),
            TriggerKind::Event,
            Some("trigger-rw-1-config-doc"),
            Some("src-rw-1"),
            Some("corr-rw-1"),
            Some(&context),
            "prompt",
            None,
            "test-fire-1",
            None,
            None,
        )
        .await
        .expect("first writer");
    let error = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-rw-2"),
            TriggerKind::Event,
            Some("trigger-rw-2-config-doc"),
            Some("src-rw-2"),
            Some("corr-rw-2"),
            Some(&context),
            "prompt",
            None,
            "test-fire-2",
            None,
            None,
        )
        .await
        .expect_err("second writer must not enqueue");
    assert!(
        error.to_string().contains("unique Active ReadWrite"),
        "{error:#}"
    );
    let rows = workspace_requests(node.as_ref(), "ws-rw").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let claimable = rows
        .iter()
        .filter(|row| row["lifecycle_state"].as_str() == Some("pending"))
        .collect::<Vec<_>>();
    assert_eq!(claimable.len(), 1, "{rows:?}");
    assert_eq!(claimable[0]["request_id"].as_str(), Some(first.as_str()));
    assert_eq!(
        rows.iter()
            .filter(|row| row["lifecycle_state"].as_str() == Some("workspaceBindingPending"))
            .count(),
        1,
        "{rows:?}"
    );
}

async fn claimed_trigger_request(
    node: &Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    node_did: &str,
) -> crate::request::lifecycle::RequestLifecycle {
    create_request(
        node,
        request_id,
        node_did,
        "pending",
        "latest",
        TriggerKind::Event,
        request_id,
    )
    .await;
    let result = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"#,
            escape_graphql_string(request_id),
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&result, "AgentRequest")
            .unwrap()
            .unwrap();
    let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        node_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    lifecycle
}

async fn canonical_partial_streams(
    node: &defra_node::EmbeddedNode,
    row: &gents_protocol::row::AgentRequestRow,
) -> (String, String) {
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::live::reconstruct_dense_prefix;
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::StreamPayload;

    let doc_id = row.doc_id.as_deref().unwrap();
    let result = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(doc_id),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let segments = result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    let observed = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let mut coordinates = Vec::new();
    for segment in &segments {
        let coordinate = (&segment.segment.source, &segment.segment.writer);
        if !coordinates.contains(&coordinate) {
            coordinates.push(coordinate);
        }
    }
    let mut text = String::new();
    let mut reasoning = String::new();
    for (source, writer) in coordinates {
        let prefix = reconstruct_dense_prefix(&observed, doc_id, source, writer, None)
            .expect("superseded partial remains a valid canonical prefix");
        for stream in prefix.streams {
            match stream.declaration.payload {
                StreamPayload::Text => text.push_str(&stream.text),
                StreamPayload::Reasoning | StreamPayload::ReasoningSummary => {
                    reasoning.push_str(&stream.text)
                }
                _ => {}
            }
        }
    }
    (text, reasoning)
}

#[tokio::test]
async fn latest_only_revokes_live_execution_and_terminalizes_response_atomically() {
    let (node, materializer) = materializer_with_node().await;
    let node_did = "did:key:z-execution-owner";
    for state in ["claimed", "processing"] {
        let request_id = format!("live-{state}");
        let mut lifecycle = claimed_trigger_request(&node, &request_id, node_did).await;
        let writer = crate::request::streaming::DefraStreamWriter::new(
            node.clone(),
            node_did,
            std::time::Duration::ZERO,
        );
        if state == "processing" {
            lifecycle.begin_owned_execution(&writer).await.unwrap();
            let doc_id = lifecycle.request().doc_id.clone();
            writer
                .start_provider_attempt(&doc_id, 0, 0, "inference.1".parse().unwrap())
                .await;
            writer
                .flush_native_partial(
                    &lifecycle,
                    &gents_protocol::message::Message::Assistant {
                        id: None,
                        content: vec![
                            gents_protocol::message::AssistantContent::Reasoning(
                                gents_protocol::message::Reasoning::new("durable reasoning"),
                            ),
                            gents_protocol::message::AssistantContent::Text(
                                gents_protocol::message::Text::from("durable partial text"),
                            ),
                        ],
                    },
                )
                .await
                .unwrap();
        }
        let before = lifecycle.request().execution_generation.clone();
        assert_eq!(
            materializer
                .supersede_active_runtime_requests_for_trigger(node_did, "latest", None)
                .await
                .unwrap(),
            1
        );
        let result = node
            .execute(&format!(
                r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                _docID request_id node_did requester_did session_id lifecycle_state
                execution_generation terminal_output terminalized_at
            }}
        }}"#
            ))
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        let data = result.data.as_ref().unwrap();
        assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "superseded");
        let terminal: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(data["AgentRequest"][0].clone()).unwrap();
        assert_ne!(terminal.execution_generation, before);
        assert_eq!(
            terminal.terminal_output,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        );
        if state == "processing" {
            let (text, reasoning) = canonical_partial_streams(&node, &terminal).await;
            assert_eq!(text, "durable partial text");
            assert_eq!(reasoning, "durable reasoning");
        }
        assert_eq!(
            materializer
                .supersede_active_runtime_requests_for_trigger(node_did, "latest", None)
                .await
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn admitted_event_replay_precedes_render_and_latest_only_supersession() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_agent_and_schedules(integration_test_agent("general"), RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot.clone());
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: snapshot.local_did.clone(),
        trigger_id: "replay-trigger".into(),
        source_collection: "ReplayWork".into(),
        source_doc_id: "physical-source".into(),
    };
    let key = crate::triggers::engine::durable::fire_key(&identity);
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: key.clone(),
        request_id: identity.request_id(),
        identity,
        task_id: "replay-task".into(),
        session_id: "original-session".into(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2030-01-01T00:00:00Z".into(),
    };
    crate::config_client::ConfigAccess::transact_local_write(&node, None, "test.seed_committed_replay", |txn| {
        let fire = fire.clone();
        Box::pin(async move {
            assert!(crate::triggers::engine::durable::stage_fire_receipt(txn, &fire).await?);
            txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) {create_AgentRequest(input: $input) {_docID}}", &serde_json::json!({"input": {
                "request_id": fire.request_id, "node_did": fire.identity.owner_did,
                "session_id": fire.session_id, "agent_id": "general", "purpose": "normal",
                "lifecycle_state": "processing", "caused_by_trigger_id": "replay-trigger",
                "created_at": fire.created_at
            }})).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let (_, rx) = watch::channel(snapshot);
    let engine = TriggerEngine::new(rx, Arc::new(materializer));
    let mut task = resolved_task("{{ doc.field_added_after_admission }}");
    task.task_id = "replay-task".into();
    task.agent_id = "general".into();
    let result = engine.dispatch(FireIntent {
        trigger_id: Some("replay-trigger".into()), trigger_kind: TriggerKind::Event,
        task, concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({"source_collection":"ReplayWork", "source_doc_id":"physical-source"}),
        doc_vars: None, correlation: None, group_vars: None, trigger_context: None, args_vars: None,
        durable_fire_key: "replayed-arrival".into(), pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    }).await;
    assert!(
        matches!(result, FireResult::Duplicate { request_id } if request_id == fire.request_id)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{lifecycle_state}} }}",
            escape_graphql_string(&fire.request_id)
        ),
        "check replay did not supersede",
    )
    .await
    .unwrap();
    assert_eq!(
        response.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
        "processing"
    );
}

#[tokio::test]
async fn admitted_schedule_replay_precedes_render_and_latest_only_supersession() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_agent_and_schedules(integration_test_agent("general"), RapidHashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot.clone());
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: snapshot.local_did.clone(),
        trigger_id: "replay-trigger".into(),
        source_collection: "Schedule".into(),
        source_doc_id: "persisted-due-occurrence".into(),
    };
    let key = crate::triggers::engine::durable::fire_key(&identity);
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: key.clone(),
        request_id: identity.request_id(),
        identity,
        task_id: "replay-task".into(),
        session_id: "original-session".into(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2030-01-01T00:00:00Z".into(),
    };
    crate::config_client::ConfigAccess::transact_local_write(&node, None, "test.seed_committed_replay", |txn| {
        let fire = fire.clone();
        Box::pin(async move {
            assert!(crate::triggers::engine::durable::stage_fire_receipt(txn, &fire).await?);
            txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) {create_AgentRequest(input: $input) {_docID}}", &serde_json::json!({"input": {
                "request_id": fire.request_id, "node_did": fire.identity.owner_did,
                "session_id": fire.session_id, "agent_id": "general", "purpose": "normal",
                "lifecycle_state": "processing", "caused_by_trigger_id": "replay-trigger",
                "created_at": fire.created_at
            }})).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let (_, rx) = watch::channel(snapshot);
    let engine = TriggerEngine::new(rx, Arc::new(materializer));
    let mut task = resolved_task("{{ doc.field_added_after_admission }}");
    task.task_id = "replay-task".into();
    task.agent_id = "general".into();
    let result = engine.dispatch(FireIntent {
        trigger_id: Some("replay-trigger".into()), trigger_kind: TriggerKind::Schedule,
        task, concurrency: ConcurrencyMode::LatestOnly,
        event_vars: serde_json::json!({"source_collection":"ReplayWork", "source_doc_id":"physical-source"}),
        doc_vars: None, correlation: None, group_vars: None, trigger_context: None, args_vars: None,
        durable_fire_key: "persisted-due-occurrence".into(), pre_materialized_request_id: None,
        on_result: Box::new(|_| {}),
    }).await;
    assert!(
        matches!(result, FireResult::Duplicate { request_id } if request_id == fire.request_id)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{lifecycle_state}} }}",
            escape_graphql_string(&fire.request_id)
        ),
        "check replay did not supersede",
    )
    .await
    .unwrap();
    assert_eq!(
        response.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
        "processing"
    );
}

#[tokio::test]
async fn scheduled_target_admission_is_atomic_and_recovers_same_occurrence() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_agent("general");
    let owner = behavior.node_did().to_string();
    for (target, agent, closed) in [
        ("captain", "general", false),
        ("wrong-agent", "other", false),
        ("closed", "general", true),
        ("foreign-owner", "general", false),
    ] {
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &node,
            target,
            if target == "foreign-owner" {
                "did:key:foreign"
            } else {
                &owner
            },
            agent,
            Some(&owner),
        )
        .await
        .unwrap();
        if closed {
            crate::session::close_session(&node, &owner, target, Some(&owner))
                .await
                .unwrap();
        }
    }
    let task = resolved_task("wake {{ session.session_id }}");
    for target in [
        "captain",
        "wrong-agent",
        "closed",
        "missing",
        "foreign-owner",
    ] {
        let trigger = format!("wake-{target}");
        let mut schedule = resolved_schedule(&trigger, task.clone());
        schedule.session_id_template = Some(target.into());
        schedule.concurrency = ConcurrencyMode::QueuedSerial;
        let snapshot = snapshot_with_agent_and_schedules(
            behavior.clone(),
            RapidHashMap::from_iter([(trigger.clone(), schedule)]),
        );
        let (tx, rx) = watch::channel(snapshot);
        let materializer = Arc::new(ProductionMaterializer::new(node.clone(), rx.clone()));
        let engine = TriggerEngine::new(rx, materializer.clone());
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: owner.clone(),
            trigger_id: trigger.clone(),
            source_collection: "Schedule".into(),
            source_doc_id: "due-2030-01-01".into(),
        };
        let intent = || FireIntent {
            trigger_id: Some(trigger.clone()),
            trigger_kind: TriggerKind::Schedule,
            task: task.clone(),
            concurrency: ConcurrencyMode::QueuedSerial,
            event_vars: serde_json::json!({}),
            doc_vars: None,
            correlation: None,
            group_vars: None,
            trigger_context: None,
            args_vars: None,
            durable_fire_key: "due-2030-01-01".into(),
            pre_materialized_request_id: None,
            on_result: Box::new(|_| {}),
        };
        let first = engine.dispatch(intent()).await;
        if target == "captain" {
            assert!(
                matches!(first, FireResult::Fired { ref request_id } if request_id == &identity.request_id()),
                "{first:?}"
            );
            let mut queued = intent();
            queued.durable_fire_key = "due-2030-01-01-second".into();
            let queued_id = gents_protocol::trigger_delivery::FireIdentity {
                source_doc_id: queued.durable_fire_key.clone(),
                ..identity.clone()
            }
            .request_id();
            assert!(matches!(
                engine.dispatch(queued).await,
                FireResult::Fired { .. }
            ));
            let response = crate::graphql::graphql_with_transaction_retry(
                &node,
                &format!(
                    "{{ AgentRequest {{ {} }} }}",
                    crate::request::watcher::AGENT_REQUEST_FIELDS
                ),
                "test.scheduled_claim_rows",
            )
            .await
            .unwrap();
            let rows: Vec<gents_protocol::row::AgentRequestRow> =
                serde_json::from_value(response.data.unwrap()["AgentRequest"].clone()).unwrap();
            let cases = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
            let mut claim_owners = Vec::new();
            for (request_id, case_name) in [
                (identity.request_id(), "serial_first_claims"),
                (queued_id, "serial_second_waits"),
            ] {
                let row = rows
                    .iter()
                    .find(|row| row.request_id == request_id)
                    .unwrap()
                    .clone();
                let request = crate::request::watcher::AgentRequest::try_from(row).unwrap();
                let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
                    node.clone(),
                    "general",
                    &owner,
                    request,
                    60,
                    crate::request::lifecycle::ExecutionOrigin::Scheduled,
                    "test-backend",
                );
                let now = chrono::Utc::now();
                let claimed = lifecycle
                    .claim_pending_durable_with_inputs(
                        || now,
                        || (now, uuid::Uuid::new_v4().to_string()),
                    )
                    .await
                    .unwrap()
                    .was_claimed();
                let expected = cases["trigger_delivery"]["queues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|case| case["name"] == case_name)
                    .unwrap()["can_claim"]
                    .as_bool()
                    .unwrap();
                assert_eq!(
                    claimed, expected,
                    "scheduled delivery uses the canonical claim queue: {case_name}"
                );
                if claimed {
                    claim_owners.push(lifecycle);
                }
            }
            tx.send(snapshot_with_agent_and_schedules(
                behavior.clone(),
                RapidHashMap::new(),
            ))
            .unwrap();
            let replay = engine.dispatch(intent()).await;
            assert!(
                matches!(&replay, FireResult::Duplicate { request_id } if request_id == &identity.request_id()),
                "{replay:?}"
            );
        } else {
            assert!(
                matches!(
                    first,
                    FireResult::Errored { .. } | FireResult::Rejected { .. }
                ),
                "{first:?}"
            );
        }
        let response = node.execute(&format!(r#"{{ TriggerFire(filter: {{fire_key: {{_eq: "{}"}}}}) {{ request_id queued_serial }} AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ session_id caused_by_trigger_kind content }} }}"#,
            escape_graphql_string(&identity.fire_key()), escape_graphql_string(&identity.request_id()))).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = response.data.unwrap();
        let expected = usize::from(target == "captain");
        assert_eq!(data["TriggerFire"].as_array().unwrap().len(), expected);
        assert_eq!(data["AgentRequest"].as_array().unwrap().len(), expected);
        if expected == 1 {
            let request = &data["AgentRequest"][0];
            assert_eq!(request["session_id"], "captain");
            assert_eq!(request["caused_by_trigger_kind"], "schedule");
            assert_eq!(data["TriggerFire"][0]["queued_serial"], true);
            assert_eq!(
                request["content"],
                "[scheduled trigger:wake-captain · task:t1 · at:]\n\nwake captain"
            );
        }
    }
}
