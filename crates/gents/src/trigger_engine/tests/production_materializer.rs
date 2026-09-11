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
        hooks: Vec::new(),
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
                request_id agent_did session_id retry_key lifecycle_state content
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
        hooks: Vec::new(),
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
                behavior_id: "general",
                session_id: "{request_id}",
                content: "production marker test",
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

/// All lifecycle states are durable markers, and the full delivery key keeps
/// membership generations separate without adding a second persisted marker.
#[tokio::test]
async fn durable_group_marker_preserves_generation_owner_and_goal_mode() {
    let (node, materializer) = materializer_with_node().await;
    let owner = "did:key:z-marker-owner";
    let trigger = "review-\"verify";
    let correlation = "run-\"42";
    let old = crate::trigger_engine::durable_fire_key("event-group", &["old-membership"]);
    let next = crate::trigger_engine::durable_fire_key("event-group", &["new-membership"]);
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
    let later = crate::trigger_engine::durable_fire_key("event-group", &["third-membership"]);
    assert!(!materializer
        .has_materialized_group_request(owner, trigger, &later)
        .await
        .unwrap());
}

#[tokio::test]
async fn materializer_rejects_workspace_from_different_explicit_owner() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let behavior = integration_test_behavior("general");
    insert_ready_workspace(
        &node,
        "ws-owner",
        "did:key:z-correct-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
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
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error:#}");
    assert!(
        workspace_requests(&node, "ws-owner").await.is_empty(),
        "wrong owner must reject before request publication"
    );
}

/// Canonical per-document gates survive source-kind edits. Correlation is
/// lineage here; only owner and logical Trigger ID define the concurrency scope.
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
        task_id: "task-ws".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "patch".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    }
}

fn writer_context(workspace_id: &str, owner: &str) -> String {
    serde_json::json!({"version":1,"source_fields":{"workspace_id":workspace_id,"workspace_owner_agent_did":owner,"workspace_authority":"readWrite"}}).to_string()
}

async fn insert_ready_workspace(
    node: &defra_node::EmbeddedNode,
    workspace_id: &str,
    owner: &str,
    writer_principal: &str,
) {
    let mutation = crate::workspace::isolated_workspace_upsert_mutation(
        &crate::workspace::IsolatedWorkspaceDoc {
            path_capability: crate::workspace::WorkspacePathCapability::exact_paths(vec![])
                .unwrap(),
            workspace_id: workspace_id.into(),
            work_unit_id: "unit-1".into(),
            repository_id: "repo-1".into(),
            base_sha: "abc".into(),
            branch: "topic".into(),
            creation_policy: "git_worktree_diff".into(),
            adapter: "git_worktree".into(),
            owner_agent_did: owner.into(),
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
                agent_did
                lifecycle_state
                workspace_id
                workspace_seal_hash
                workspace_owner_agent_did
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
async fn materializer_rejects_missing_workspace_owner_without_actor_fallback() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let behavior = integration_test_behavior("general");
    // Even an actor-owned workspace cannot repair an incomplete source tuple.
    insert_ready_workspace(
        &node,
        "ws-incomplete",
        behavior.agent_did(),
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(node.clone(), rx);
    let context=serde_json::json!({"version":1,"source_fields":{"workspace_id":"ws-incomplete","workspace_authority":"readWrite"}}).to_string();
    let error = materializer
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
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("owner principal and authority together"),
        "{error:#}"
    );
    assert!(workspace_requests(&node, "ws-incomplete").await.is_empty());
}

#[tokio::test]
async fn materializer_preserves_explicit_workspace_owner_distinct_from_executor() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    let executor = behavior.agent_did().to_owned();
    assert_ne!(executor, "did:key:z-workspace-owner");
    insert_ready_workspace(
        node.as_ref(),
        "ws-stamp",
        "did:key:z-workspace-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
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
        )
        .await
        .expect("explicit workspace owner survives request publication");
    let rows = workspace_requests(node.as_ref(), "ws-stamp").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["agent_did"].as_str(), Some(executor.as_str()));
    assert_eq!(rows[0]["workspace_id"].as_str(), Some("ws-stamp"));
    assert_eq!(rows[0]["workspace_authority"].as_str(), Some("readWrite"));
    assert_eq!(rows[0]["request_id"].as_str(), Some(request_id.as_str()));
    assert_eq!(
        rows[0]["workspace_owner_agent_did"].as_str(),
        Some("did:key:z-workspace-owner")
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
        "did:key:z-workspace-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
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
    assert_eq!(
        rows[0]["workspace_owner_agent_did"].as_str(),
        Some("did:key:z-workspace-owner")
    );
    assert_eq!(rows[0]["lifecycle_state"].as_str(), Some("pending"));
}

#[tokio::test]
async fn unique_read_write_denial_does_not_leave_claimable_request() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let behavior = integration_test_behavior("general");
    insert_ready_workspace(
        node.as_ref(),
        "ws-rw",
        "did:key:z-workspace-owner",
        behavior.agent_did(),
    )
    .await;
    let snapshot = snapshot_with_behavior_and_schedules(behavior, HashMap::new());
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

#[tokio::test]
async fn latest_only_revokes_live_execution_and_terminalizes_response_atomically() {
    let (node, materializer) = materializer_with_node().await;
    let agent_did = "did:key:z-execution-owner";
    for state in ["claimed", "processing"] {
        let request_id = format!("live-{state}");
        let expiry = (Utc::now() + ChronoDuration::minutes(5)).to_rfc3339();
        create_request(
            &node,
            &request_id,
            agent_did,
            state,
            "latest",
            TriggerKind::Event,
            state,
        )
        .await;
        let result = node.execute(&format!(r#"mutation {{ update_AgentRequest(
            filter: {{ request_id: {{ _eq: "{request_id}" }} }},
            input: {{ execution_generation: "original", execution_lease_expires_at: "{expiry}", execution_progress_seq: 7 }}
        ) {{ _docID }} }}"#)).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        let request_doc_id = result.data.as_ref().unwrap()["update_AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap();
        if state == "processing" {
            let result = node.execute(&format!(r#"mutation {{ create_AgentResponse(input: {{
                response_key: "{request_id}", request_id: "{request_id}", request_doc_id: "{request_doc_id}",
                agent_did: "{agent_did}", session_id: "{request_id}", behavior_id: "general",
                status: "streaming", content: "durable partial text", reasoning: "durable reasoning"
            }}) {{ _docID }} }}"#)).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        assert_eq!(
            materializer
                .supersede_active_runtime_requests_for_trigger(agent_did, "latest", None)
                .await
                .unwrap(),
            1
        );
        let result = node.execute(&format!(r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state execution_generation }}
            AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ status content reasoning }}
        }}"#)).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        let data = result.data.as_ref().unwrap();
        assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "superseded");
        assert_ne!(data["AgentRequest"][0]["execution_generation"], "original");
        assert_eq!(data["AgentResponse"].as_array().unwrap().len(), 1);
        assert_eq!(data["AgentResponse"][0]["status"], "error");
        if state == "processing" {
            assert_eq!(data["AgentResponse"][0]["content"], "durable partial text");
            assert_eq!(data["AgentResponse"][0]["reasoning"], "durable reasoning");
        }
        assert_eq!(
            materializer
                .supersede_active_runtime_requests_for_trigger(agent_did, "latest", None)
                .await
                .unwrap(),
            0
        );
    }
}
