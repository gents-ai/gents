use super::*;

#[tokio::test]
async fn goal_control_failures_pause_after_two_retries_even_with_generated_text() {
    let db = test_db("goal-control-failure-loop").await;
    let mut parent_id = "control-parent".to_string();
    let (user, agent, access) = goal_recovery_actors(&db, &parent_id, "completed").await;
    let mut parent_doc = access
        .execute("{AgentRequest(filter: {request_id: {_eq: \"control-parent\"}}) {_docID}}")
        .await
        .unwrap()["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Preserve the objective without repeating failed control calls"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let (mut source, _snapshot) = source_for_agent(&db, &agent).await;
    for retry in 0..=gents::goal::MAX_INFRASTRUCTURE_RETRIES {
        failed_control_as(
            &access,
            &agent,
            user.did(),
            &parent_doc,
            &parent_id,
            "get_goal",
            db.node_identity.did(),
        )
        .await;
        failed_control_as(
            &access,
            &agent,
            user.did(),
            &parent_doc,
            &parent_id,
            "update_goal",
            db.node_identity.did(),
        )
        .await;
        if retry == gents::goal::MAX_INFRASTRUCTURE_RETRIES {
            let paused = wait_for_goal_transition_as(
                &access,
                &db.node,
                db.node_identity.did(),
                &mut source,
                "bounded control failure pause",
                |goal| goal.parsed_status() == Some(GoalStatus::Paused),
            )
            .await;
            assert_eq!(paused.objective, goal.objective);
            assert_eq!(paused.infrastructure_retry_count.unwrap_or_default(), retry);
            assert!(paused
                .last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("Goal control attribution"));
            assert_eq!(goal_children_from_access(&access).await.len() as i64, retry);
            drop(source);
            let (mut restarted, _snapshot) = source_for_agent(&db, &agent).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(150), restarted.next_fire())
                    .await
                    .is_err()
            );
            assert_eq!(goal_children_from_access(&access).await.len() as i64, retry);
            let recovered = gents::goal::resume_goal_request(
                &db.client_access(&user),
                &user,
                db.node_identity.did(),
                SESSION,
                &parent_id,
            )
            .await
            .unwrap();
            assert!(recovered.created);
            publish_owned_terminal_activity(
                &access,
                agent.as_ref(),
                &recovered.doc_id,
                "Goal control is healthy; useful work resumed.",
            )
            .await;
            set_request_state_as(&access, &recovered.doc_id, "completed").await;
            tokio::time::timeout(Duration::from_secs(3), restarted.next_fire())
                .await
                .expect("healthy continuation deadline")
                .expect("normal continuation after recovery");
            let healthy = load_goal_as(&access, db.node_identity.did()).await.unwrap();
            assert_eq!(healthy.parsed_status(), Some(GoalStatus::Active));
            assert_eq!(healthy.infrastructure_retry_count.unwrap_or_default(), 0);
            assert_eq!(healthy.objective, goal.objective);
            assert_eq!(
                goal_children_from_access(&access).await.len() as i64,
                retry + 2
            );
            return;
        }
        let intent = tokio::time::timeout(Duration::from_secs(3), source.next_fire())
            .await
            .expect("bounded retry deadline")
            .expect("one recovery child");
        parent_id = intent
            .pre_materialized_request_id
            .expect("materialized retry");
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len() as i64, retry + 1);
        let child = children
            .iter()
            .find(|row| row.request_id == parent_id)
            .unwrap();
        parent_doc = child.doc_id.clone();
        publish_owned_terminal_activity(
            &access,
            agent.as_ref(),
            &parent_doc,
            "The Goal control is still unavailable; no work changed.",
        )
        .await;
        set_request_state_as(&access, &parent_doc, "completed").await;
    }
    panic!("the retry bound did not stop continuation");
}

#[tokio::test]
async fn unrelated_or_foreign_tool_failures_do_not_consume_goal_control_retries() {
    for (tool, foreign) in [("bash", false), ("get_goal", true)] {
        let db = test_db("goal-control-failure-scope").await;
        let (user, agent, access) = goal_recovery_actors(&db, "ordinary-parent", "completed").await;
        let parent = access
            .execute("{AgentRequest(filter: {request_id: {_eq: \"ordinary-parent\"}}) {_docID}}")
            .await
            .unwrap()["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        set_goal_from_access(
            &access,
            db.node_identity.did(),
            SESSION,
            Some("Continue useful work"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let keys = tempfile::tempdir().unwrap();
        let foreign_node =
            gents::KeyIdentity::load_or_create(keys.path().join("foreign-node.key"), None).unwrap();
        let scope = if foreign {
            foreign_node.did()
        } else {
            db.node_identity.did()
        };
        failed_control_as(
            &access,
            &agent,
            user.did(),
            &parent,
            "ordinary-parent",
            tool,
            scope,
        )
        .await;
        let (mut source, _snapshot) = source_for_agent(&db, &agent).await;
        tokio::time::timeout(Duration::from_secs(3), source.next_fire())
            .await
            .expect("ordinary continuation deadline")
            .expect("ordinary continuation");
        let observed = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(observed.parsed_status(), Some(GoalStatus::Active));
        assert_eq!(observed.infrastructure_retry_count.unwrap_or_default(), 0);
        assert_eq!(goal_children_from_access(&access).await.len(), 1);
    }
}

async fn failed_control_as(
    access: &ConfigAccess,
    agent: &Arc<dyn NodeIdentity>,
    requester: &str,
    parent: &str,
    request_id: &str,
    tool: &str,
    scope: &str,
) {
    let request = access
        .execute(&format!(
            r#"{{AgentRequest(docID: "{}") {{session_doc_id}}}}"#,
            escape_graphql_string(parent)
        ))
        .await
        .unwrap();
    let session = request["data"]["AgentRequest"][0]["session_doc_id"]
        .as_str()
        .unwrap();
    let key = format!("{request_id}-{tool}-{scope}");
    let doc = access.transact_write("test.goal.failed_control", |txn| {
        let key = &key;
        Box::pin(async move {
            let input = serde_json::json!({"tool_call_key":key,"tool_call_id":key,
                "node_did":scope,"requester_did":requester,"session_id":SESSION,"session_doc_id":session,
                "agent_did":agent.did(),"creator_did":agent.did(),"parent_linked":true,
                "request_id":request_id,"request_doc_id":parent,"tool_name":tool,
                "status":"completed","lifecycle_state":"failed","tool_failure_class":"serviceUnavailable"});
            let result = txn.execute_with_variables("mutation($input: AgentToolCallMutationInputArg!) {create_AgentToolCall(input:$input) {_docID}}", &serde_json::json!({"input": input})).await?;
            let doc = gents::store::graphql::created_doc_id(&result, "AgentToolCall")?;
            txn.add_resource_relationship("AgentToolCall", &doc, "parent", "sessions", session).await?;
            txn.add_actor_relationship("AgentToolCall", &doc, "working_agent", &agent.did().parse()?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap();
    let observed = access
        .execute(&format!(
            r#"{{AgentToolCall(docID: "{}") {{_docID node_did request_doc_id tool_name}}}}"#,
            escape_graphql_string(&doc)
        ))
        .await
        .unwrap();
    let row = &observed["data"]["AgentToolCall"][0];
    assert_eq!(row["_docID"], doc);
    assert_eq!(row["node_did"], scope);
    assert_eq!(row["request_doc_id"], parent);
    assert_eq!(row["tool_name"], tool);
}
