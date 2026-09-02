use super::*;

async fn materializer_with_node() -> (Arc<defra_node::EmbeddedNode>, ProductionMaterializer) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_behavior_and_schedules(integration_test_behavior("general"), HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    (node, materializer)
}

#[tokio::test]
async fn goal_task_materialization_is_atomic_and_idempotent_for_one_durable_fire() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    let agent_did = behavior.agent_did().to_string();
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let task = ResolvedTask {
        task_id: "task-goal-release".to_string(),
        name: Some("release task".to_string()),
        behavior_id: "general".to_string(),
        prompt_template: "implement release".to_string(),
        goal_objective_template: Some("ship release".to_string()),
        goal_token_budget: Some(4_096),
        output_schema_ref: None,
    };
    let fire_key = "event:release-trigger:doc:release-42";
    let identity = crate::goal::task_goal_fire_identity(&agent_did, &task.task_id, fire_key);

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
        )
        .await
        .expect_err("an invalid declaration must roll back before publication");
    assert!(invalid.to_string().contains("non-empty"), "{invalid:#}");
    assert!(
        crate::goal::load_canonical_goal(node.as_ref(), &agent_did, &identity.session_id)
            .await
            .unwrap()
            .is_none(),
        "failed materialization must not leave a Goal"
    );

    let first = materializer
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
        )
        .await
        .expect("goal-backed Task fire");
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

    let goal = crate::goal::load_canonical_goal(node.as_ref(), &agent_did, &identity.session_id)
        .await
        .unwrap()
        .expect("Goal must commit with its first request");
    assert_eq!(goal.objective, "ship release");
    assert_eq!(goal.token_budget, Some(4_096));
    assert_eq!(goal.status, "active");

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
                request_id agent_did session_id retry_key status lifecycle_state content
                caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind
                caused_by_source_doc_id caused_by_correlation admission_kind
                runtime_source_request_id runtime_source_kind
            }}
            GoalCreationClaim(filter: {{ creation_key: {{ _eq: "{}" }} }}) {{
                agent_did session_id objective token_budget
            }}
        }}"#,
        escape_graphql_string(&identity.retry_key),
        escape_graphql_string(&crate::goal::deterministic_goal_creation_key(
            &agent_did,
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
        request["request_id"].as_str(),
        Some(identity.request_id.as_str())
    );
    assert_eq!(request["agent_did"].as_str(), Some(agent_did.as_str()));
    assert_eq!(
        request["session_id"].as_str(),
        Some(identity.session_id.as_str())
    );
    assert_eq!(
        request["retry_key"].as_str(),
        Some(identity.retry_key.as_str())
    );
    assert_eq!(request["status"].as_str(), Some("pending"));
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
        crate::goal::delete_goals_for_session(node.as_ref(), &agent_did, &identity.session_id)
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
async fn goal_task_identity_and_recovery_are_scoped_by_agent_did() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let first_behavior = integration_test_behavior("general");
    let first_did = first_behavior.agent_did().to_string();
    let first_snapshot = snapshot_with_behavior_and_schedules(first_behavior, HashMap::new());
    let (_first_tx, first_rx) = watch::channel(first_snapshot);
    let first_materializer = ProductionMaterializer::new(node.clone(), first_rx);
    let second_behavior = integration_test_behavior("general");
    let second_did = second_behavior.agent_did().to_string();
    let second_snapshot = snapshot_with_behavior_and_schedules(second_behavior, HashMap::new());
    let (_second_tx, second_rx) = watch::channel(second_snapshot);
    let second_materializer = ProductionMaterializer::new(node, second_rx);
    let task = ResolvedTask {
        task_id: "shared-task-id".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "shared prompt".to_string(),
        goal_objective_template: Some("shared objective".to_string()),
        goal_token_budget: None,
        output_schema_ref: None,
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
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    let agent_did = behavior.agent_did().to_string();
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx);
    let task = ResolvedTask {
        task_id: "foreign-collision-task".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "prompt".to_string(),
        goal_objective_template: Some("objective".to_string()),
        goal_token_budget: None,
        output_schema_ref: None,
    };
    let fire_key = "foreign-collision-fire";
    let identity = crate::goal::task_goal_fire_identity(&agent_did, &task.task_id, fire_key);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{}",
                agent_did: "did:key:foreign-task-owner",
                behavior_id: "general",
                session_id: "{}",
                retry_key: "{}",
                content: "foreign collision",
                status: "pending",
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
    agent_did: &str,
    lifecycle_state: &str,
    trigger_id: &str,
    trigger_kind: TriggerKind,
    correlation: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let agent_did = escape_graphql_string(agent_did);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let trigger_id = escape_graphql_string(trigger_id);
    let correlation = escape_graphql_string(correlation);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                content: "production marker test",
                status: "{lifecycle_state}",
                lifecycle_state: "{lifecycle_state}",
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

/// The AgentRequest lineage tuple is the durable at-most-once marker for a
/// correlated group. Exercise the exact production GraphQL query against a
/// terminal request so this test also proves that every lifecycle state is a
/// marker, not only requests which remain active.
#[tokio::test]
async fn durable_group_marker_matches_all_four_lineage_discriminators() {
    let (node, materializer) = materializer_with_node().await;
    let agent_did = "did:key:z-marker-owner";
    let trigger_id = "review-\"verify";
    let correlation = "run-\"42";
    create_request(
        node.as_ref(),
        "marker-completed",
        agent_did,
        "completed",
        trigger_id,
        TriggerKind::Event,
        correlation,
    )
    .await;

    assert!(materializer
        .has_materialized_group_request(agent_did, trigger_id, TriggerKind::Event, correlation,)
        .await
        .unwrap());
    assert!(!materializer
        .has_materialized_group_request(
            "did:key:z-other-owner",
            trigger_id,
            TriggerKind::Event,
            correlation,
        )
        .await
        .unwrap());
    assert!(!materializer
        .has_materialized_group_request(agent_did, "review-other", TriggerKind::Event, correlation,)
        .await
        .unwrap());
    assert!(!materializer
        .has_materialized_group_request(agent_did, trigger_id, TriggerKind::Schedule, correlation,)
        .await
        .unwrap());
    assert!(!materializer
        .has_materialized_group_request(agent_did, trigger_id, TriggerKind::Event, "run-other",)
        .await
        .unwrap());
}

#[tokio::test]
async fn materializer_skips_workspace_bound_request_for_other_deployment() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_behavior_and_schedules(integration_test_behavior("general"), HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer =
        ProductionMaterializer::new(node, snapshot_rx).with_local_deployment_id("deploy-replica");
    let task = ResolvedTask {
        task_id: "task-ws".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "patch".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
    };
    let context = serde_json::json!({
        "version": 1,
        "source_fields": {
            "workspace_id": "ws-1",
            "workspace_authority": "readWrite",
            "workspace_owner_deployment_id": "deploy-owner"
        }
    })
    .to_string();
    let error = materializer
        .materialize(
            &task,
            Some("trigger-ws"),
            TriggerKind::Event,
            Some("trigger-ws-config-doc"),
            Some("src-1"),
            Some("corr-1"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
        )
        .await
        .expect_err("replica must not enqueue workspace-bound work");
    assert!(
        error
            .downcast_ref::<crate::trigger_engine::MaterializeSkip>()
            .is_some(),
        "{error}"
    );
    match crate::trigger_engine::fire_result_from_materialize(Err(error)) {
        FireResult::Skipped { reason } => {
            assert!(reason.contains("another deployment"), "{reason}");
        }
        other => panic!("expected Skipped, got {other:?}"),
    }
}

/// Per-group Serial/LatestOnly gates include correlation; per-document gates
/// deliberately omit it and remain trigger-wide. Prove both query shapes and
/// the matching supersede mutation against persisted rows.
#[tokio::test]
async fn active_gate_and_supersede_honor_optional_correlation_scope() {
    let (node, materializer) = materializer_with_node().await;
    let agent_did = "did:key:z-concurrency-owner";
    let trigger_id = "review-verify";
    for (request_id, correlation) in [("active-a", "run-a"), ("active-b", "run-b")] {
        create_request(
            node.as_ref(),
            request_id,
            agent_did,
            "pending",
            trigger_id,
            TriggerKind::Event,
            correlation,
        )
        .await;
    }

    assert!(materializer
        .has_active_runtime_request_for_trigger(
            agent_did,
            trigger_id,
            TriggerKind::Event,
            Some("run-a"),
            None,
        )
        .await
        .unwrap());
    assert!(!materializer
        .has_active_runtime_request_for_trigger(
            agent_did,
            trigger_id,
            TriggerKind::Event,
            Some("run-a"),
            Some("active-a"),
        )
        .await
        .unwrap());
    assert_eq!(
        materializer
            .supersede_active_runtime_requests_for_trigger(
                agent_did,
                trigger_id,
                TriggerKind::Event,
                Some("run-a"),
                Some("active-a"),
            )
            .await
            .unwrap(),
        0,
        "a retried durable fire must not supersede its own request"
    );
    assert!(!materializer
        .has_active_runtime_request_for_trigger(
            agent_did,
            trigger_id,
            TriggerKind::Event,
            Some("run-missing"),
            None,
        )
        .await
        .unwrap());
    assert!(
        materializer
            .has_active_runtime_request_for_trigger(
                agent_did,
                trigger_id,
                TriggerKind::Event,
                None,
                None,
            )
            .await
            .unwrap(),
        "omitting correlation must preserve trigger-wide per-document gating"
    );

    assert_eq!(
        materializer
            .supersede_active_runtime_requests_for_trigger(
                agent_did,
                trigger_id,
                TriggerKind::Event,
                Some("run-a"),
                None,
            )
            .await
            .unwrap(),
        1
    );
    assert!(!materializer
        .has_active_runtime_request_for_trigger(
            agent_did,
            trigger_id,
            TriggerKind::Event,
            Some("run-a"),
            None,
        )
        .await
        .unwrap());
    assert!(
        materializer
            .has_active_runtime_request_for_trigger(
                agent_did,
                trigger_id,
                TriggerKind::Event,
                Some("run-b"),
                None,
            )
            .await
            .unwrap(),
        "correlated supersede must leave sibling groups active"
    );
    assert_eq!(
        materializer
            .supersede_active_runtime_requests_for_trigger(
                agent_did,
                trigger_id,
                TriggerKind::Event,
                None,
                None,
            )
            .await
            .unwrap(),
        1,
        "omitting correlation must supersede all remaining trigger-wide rows"
    );
}

fn workspace_writer_task() -> ResolvedTask {
    ResolvedTask {
        task_id: "task-ws".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "patch".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
    }
}

fn writer_context(workspace_id: &str, owner_field: &str, owner: &str) -> String {
    let mut source_fields = serde_json::Map::new();
    source_fields.insert(
        "workspace_id".into(),
        serde_json::Value::String(workspace_id.into()),
    );
    source_fields.insert(
        "workspace_authority".into(),
        serde_json::Value::String("readWrite".into()),
    );
    source_fields.insert(owner_field.into(), serde_json::Value::String(owner.into()));
    serde_json::json!({
        "version": 1,
        "source_fields": source_fields
    })
    .to_string()
}

async fn insert_ready_workspace(
    node: &defra_node::EmbeddedNode,
    workspace_id: &str,
    owner: &str,
    writer_principal: &str,
) {
    let mutation = crate::workspace::isolated_workspace_upsert_mutation(
        &crate::workspace::IsolatedWorkspaceDoc {
            workspace_id: workspace_id.into(),
            work_unit_id: "unit-1".into(),
            repository_id: "repo-1".into(),
            base_sha: "abc".into(),
            branch: "topic".into(),
            creation_policy: "alwaysCreate".into(),
            adapter: "git_worktree".into(),
            owner_deployment_id: owner.into(),
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
                status
                lifecycle_state
                workspace_owner_deployment_id
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
async fn materializer_skips_callback_result_owner_deployment_id_on_replica() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let snapshot =
        snapshot_with_behavior_and_schedules(integration_test_behavior("general"), HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer =
        ProductionMaterializer::new(node, snapshot_rx).with_local_deployment_id("deploy-replica");
    let context = writer_context("ws-1", "owner_deployment_id", "deploy-owner");
    let error = materializer
        .materialize(
            &workspace_writer_task(),
            Some("trigger-ws"),
            TriggerKind::Event,
            Some("trigger-ws-config-doc"),
            Some("src-1"),
            Some("corr-1"),
            Some(&context),
            "prompt",
            None,
            "test-fire",
        )
        .await
        .expect_err("replica must skip CallbackResult owner_deployment_id");
    assert!(
        error
            .downcast_ref::<crate::trigger_engine::MaterializeSkip>()
            .is_some(),
        "{error}"
    );
}

#[tokio::test]
async fn materializer_stamps_owner_when_trigger_context_omits_it() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    insert_ready_workspace(
        node.as_ref(),
        "ws-stamp",
        "deploy-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx)
        .with_local_deployment_id("deploy-owner");
    let context = serde_json::json!({
        "version": 1,
        "source_fields": {
            "workspace_id": "ws-stamp",
            "workspace_authority": "readWrite"
        }
    })
    .to_string();
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
        )
        .await
        .expect("owner host stamps IsolatedWorkspace owner");
    let rows = workspace_requests(node.as_ref(), "ws-stamp").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["request_id"].as_str(), Some(request_id.as_str()));
    assert_eq!(
        rows[0]["workspace_owner_deployment_id"].as_str(),
        Some("deploy-owner")
    );
}

#[tokio::test]
async fn goal_task_workspace_activation_retry_is_idempotent() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    insert_ready_workspace(
        node.as_ref(),
        "ws-goal-retry",
        "deploy-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx)
        .with_local_deployment_id("deploy-owner");
    let context = writer_context(
        "ws-goal-retry",
        "workspace_owner_deployment_id",
        "deploy-owner",
    );
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
        )
        .await
        .expect("activation acknowledgement retry");

    assert_eq!(retry, first);
    let rows = workspace_requests(node.as_ref(), "ws-goal-retry").await;
    assert_eq!(rows.len(), 1, "retry must reuse the staged request");
    assert_eq!(rows[0]["status"].as_str(), Some("pending"));
}

#[tokio::test]
async fn unique_read_write_denial_does_not_leave_claimable_request() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    insert_ready_workspace(node.as_ref(), "ws-rw", "deploy-owner", behavior.agent_did()).await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), snapshot_rx)
        .with_local_deployment_id("deploy-owner");
    let context = writer_context("ws-rw", "workspace_owner_deployment_id", "deploy-owner");
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
        .filter(|row| {
            row["status"].as_str() == Some("pending")
                && row["lifecycle_state"].as_str() == Some("pending")
        })
        .collect::<Vec<_>>();
    assert_eq!(claimable.len(), 1, "{rows:?}");
    assert_eq!(claimable[0]["request_id"].as_str(), Some(first.as_str()));
    assert_eq!(
        rows.iter()
            .filter(|row| row["status"].as_str() == Some("workspace_binding_pending"))
            .count(),
        1,
        "{rows:?}"
    );
}
