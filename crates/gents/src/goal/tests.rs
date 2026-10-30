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
            session_id: "usage-session", agent_id: "general", content: "test",
            lifecycle_state: "completed", backend_id: "",
            created_at: "2026-07-15T00:00:00Z", retry_count: 0, max_retries: 0
        }}) {{ _docID }} }}"#
    );
    let response = node.execute(&request).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let request_doc_id = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
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
                agent_id: "test", node_did: "{owner}", call_kind: "inference", attempt: 1,
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
    let usage = crate::config_client::ConfigAccess::transact_local_readonly(
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
        session_token_usage(&node, owner, "usage-session")
            .await
            .unwrap(),
        1257
    );
}

#[tokio::test]
async fn auto_resume_field_round_trips_through_the_live_schema() {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let owner = "did:key:z6MkTestOwner";
    let goal = set_goal(&node, owner, "session", Some("ship"), None, None)
        .await
        .unwrap();
    assert_eq!(goal.auto_resume_at_reset, None);
    let response = node
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{_docID: {{_eq: "{}"}}}},
            input: {{auto_resume_at_reset: true}}) {{_docID}} }}"#,
            goal.doc_id
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let stored = load_canonical_goal(&node, owner, "session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.auto_resume_at_reset, Some(true));
}

async fn auto_resume_access() -> crate::ConfigAccess {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    crate::ConfigAccess::local(node)
}

#[tokio::test]
async fn auto_resume_operator_setter_stores_it() {
    let access = auto_resume_access().await;
    let owner = "did:key:z6MkTestOwner";
    let created = set_goal_from_access(&access, owner, "s", Some("ship"), None, None, Some(true))
        .await
        .unwrap();
    assert_eq!(created.auto_resume_at_reset, Some(true));
    let off = set_goal_from_access(&access, owner, "s", None, None, None, Some(false))
        .await
        .unwrap();
    assert_eq!(off.auto_resume_at_reset, Some(false));
}

#[tokio::test]
async fn auto_resume_none_and_set_goal_keep_the_stored_value() {
    let access = auto_resume_access().await;
    let node = access.local_node().expect("local test access");
    let owner = "did:key:z6MkTestOwner";
    set_goal_from_access(&access, owner, "s", Some("ship"), None, None, Some(true))
        .await
        .unwrap();
    let kept = set_goal_from_access(
        &access,
        owner,
        "s",
        None,
        Some(GoalStatus::Paused),
        Some(Some(10)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(kept.auto_resume_at_reset, Some(true));
    let unchanged = set_goal(node, owner, "s", Some("ship more"), None, None)
        .await
        .unwrap();
    assert_eq!(unchanged.auto_resume_at_reset, Some(true));
}
