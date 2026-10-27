use super::*;

async fn failed_control(db: &TestDb, parent: &str, request_id: &str, tool: &str, scope: &str) {
    let owner = escape_graphql_string(db.node_identity.did());
    let parent = escape_graphql_string(parent);
    let request = escape_graphql_string(request_id);
    let name = escape_graphql_string(tool);
    let scope = escape_graphql_string(scope);
    let key = escape_graphql_string(&format!("{request_id}-{tool}-{scope}"));
    ConfigAccess::write_local(
        &db.node,
        "test.goal.failed_control",
        &format!(
            r#"mutation {{
        create_AgentToolCall(input: {{tool_call_key: "{key}", tool_call_id: "{key}",
            node_did: "{scope}", requester_did: "{owner}", session_id: "goal-session",
            request_id: "{request}", request_doc_id: "{parent}", tool_name: "{name}",
            status: "completed", lifecycle_state: "failed", tool_failure_class: "serviceUnavailable"
        }}) {{_docID}} }}"#
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn goal_control_failures_pause_after_two_retries_even_with_generated_text() {
    let db = test_db("goal-control-failure-loop").await;
    let mut parent_id = "control-parent".to_string();
    let mut parent_doc = seed_completed_request(&db, &parent_id).await;
    let goal = set_goal(
        &db.node,
        db.node_identity.did(),
        SESSION,
        Some("Preserve the objective without repeating failed control calls"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let (mut source, _snapshot) = source(&db).await;
    for retry in 0..=gents::goal::MAX_INFRASTRUCTURE_RETRIES {
        failed_control(
            &db,
            &parent_doc,
            &parent_id,
            "get_goal",
            db.node_identity.did(),
        )
        .await;
        failed_control(
            &db,
            &parent_doc,
            &parent_id,
            "update_goal",
            db.node_identity.did(),
        )
        .await;
        if retry == gents::goal::MAX_INFRASTRUCTURE_RETRIES {
            let paused = wait_for_goal_transition(
                &db,
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
            assert_eq!(goal_children(&db).await.len() as i64, retry);
            drop(source);
            let (mut restarted, _snapshot) = super::source(&db).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(150), restarted.next_fire())
                    .await
                    .is_err()
            );
            assert_eq!(goal_children(&db).await.len() as i64, retry);
            let recovered = gents::goal::resume_goal_request(
                &ConfigAccess::local(db.node.clone()),
                db.node_identity.as_ref(),
                db.node_identity.did(),
                SESSION,
                &parent_id,
            )
            .await
            .unwrap();
            assert!(recovered.created);
            publish_canonical_terminal_activity(
                &db,
                &recovered.doc_id,
                "Goal control is healthy; useful work resumed.",
            )
            .await;
            set_request_lifecycle_state(&db.node, &recovered.doc_id, "completed").await;
            tokio::time::timeout(Duration::from_secs(3), restarted.next_fire())
                .await
                .expect("healthy continuation deadline")
                .expect("normal continuation after recovery");
            let healthy = load_canonical_goal(&db.node, db.node_identity.did(), SESSION)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(healthy.parsed_status(), Some(GoalStatus::Active));
            assert_eq!(healthy.infrastructure_retry_count.unwrap_or_default(), 0);
            assert_eq!(healthy.objective, goal.objective);
            assert_eq!(goal_children(&db).await.len() as i64, retry + 2);
            return;
        }
        let intent = tokio::time::timeout(Duration::from_secs(3), source.next_fire())
            .await
            .expect("bounded retry deadline")
            .expect("one recovery child");
        parent_id = intent
            .pre_materialized_request_id
            .expect("materialized retry");
        let children = goal_children(&db).await;
        assert_eq!(children.len() as i64, retry + 1);
        let child = children
            .iter()
            .find(|row| row.request_id == parent_id)
            .unwrap();
        parent_doc = child.doc_id.clone();
        publish_canonical_terminal_activity(
            &db,
            &parent_doc,
            "The Goal control is still unavailable; no work changed.",
        )
        .await;
        set_request_lifecycle_state(&db.node, &parent_doc, "completed").await;
    }
    panic!("the retry bound did not stop continuation");
}

#[tokio::test]
async fn unrelated_or_foreign_tool_failures_do_not_consume_goal_control_retries() {
    for (tool, foreign) in [("bash", false), ("get_goal", true)] {
        let db = test_db("goal-control-failure-scope").await;
        let parent = seed_completed_request(&db, "ordinary-parent").await;
        set_goal(
            &db.node,
            db.node_identity.did(),
            SESSION,
            Some("Continue useful work"),
            Some(GoalStatus::Active),
            None,
        )
        .await
        .unwrap();
        let scope = if foreign {
            "did:key:foreign"
        } else {
            db.node_identity.did()
        };
        failed_control(&db, &parent, "ordinary-parent", tool, scope).await;
        let (mut source, _snapshot) = super::source(&db).await;
        tokio::time::timeout(Duration::from_secs(3), source.next_fire())
            .await
            .expect("ordinary continuation deadline")
            .expect("ordinary continuation");
        let observed = load_canonical_goal(&db.node, db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observed.parsed_status(), Some(GoalStatus::Active));
        assert_eq!(observed.infrastructure_retry_count.unwrap_or_default(), 0);
        assert_eq!(goal_children(&db).await.len(), 1);
    }
}
