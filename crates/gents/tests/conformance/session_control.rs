use super::*;

fn model_case(name: &str) -> serde_json::Value {
    lean_session_control_cases()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("Lean session-control model omitted {name}"))
        .clone()
}

#[tokio::test]
async fn generated_session_control_cases_drive_native_interrupt_stop_and_resume() {
    let interrupt_case = model_case("interrupt_idle");
    let stop_case = model_case("stop_drains");
    let resume_case = model_case("resume_terminal_goal");
    let db = test_db("session-control-generated").await;

    let interrupted_session = uuid::Uuid::new_v4().to_string();
    assert_eq!(interrupt_case["initial_pending_count"], 0);
    support::create_agent_session(
        &db.node,
        &interrupted_session,
        AGENT_NAME,
        "2026-10-20T10:00:00Z",
    )
    .await;
    gents::goal::set_goal(
        &db.node,
        NODE_DID,
        &interrupted_session,
        Some("pause on session interrupt"),
        Some(gents::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents::session_message::interrupt_session(
            &db.node,
            &gents::session_origin::SessionScope {
                node_did: NODE_DID.to_owned(),
                session_id: interrupted_session.clone(),
                requester_did: None,
            },
        )
        .await
        .unwrap(),
        None
    );
    let interrupted_goal =
        gents::goal::load_canonical_goal(&db.node, NODE_DID, &interrupted_session)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        interrupted_goal.status,
        interrupt_case["goal_status"].as_str().unwrap()
    );
    let interrupted_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(NODE_DID),
        escape_graphql_string(&interrupted_session)
    )).await;
    assert_eq!(
        interrupted_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        interrupt_case["stopped"].as_bool().unwrap()
    );

    let stopped_session = uuid::Uuid::new_v4().to_string();
    support::create_agent_session(
        &db.node,
        &stopped_session,
        AGENT_NAME,
        "2026-10-20T10:00:01Z",
    )
    .await;
    let mut pending_ids = Vec::new();
    let mut pending_docs = Vec::new();
    for index in 0..stop_case["initial_pending_count"].as_u64().unwrap() {
        let pending_id = uuid::Uuid::new_v4().to_string();
        let pending_doc = support::create_request(
            &db.node,
            &pending_id,
            &stopped_session,
            "pending",
            &format!("2026-10-20T10:00:{:02}Z", 2 + index),
        )
        .await;
        pending_ids.push(pending_id);
        pending_docs.push(pending_doc);
    }
    gents::goal::set_goal(
        &db.node,
        NODE_DID,
        &stopped_session,
        Some("stop and later resume"),
        Some(gents::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let receipt = gents::session::stop_session(&db.node, NODE_DID, &stopped_session, None)
        .await
        .unwrap();
    assert_eq!(receipt.drained_request_ids, pending_ids);
    let stopped_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(NODE_DID),
        escape_graphql_string(&stopped_session)
    )).await;
    assert_eq!(
        stopped_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        stop_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        gents::goal::load_canonical_goal(&db.node, NODE_DID, &stopped_session)
            .await
            .unwrap()
            .unwrap()
            .status,
        stop_case["goal_status"].as_str().unwrap()
    );

    for pending_doc in pending_docs {
        let lifecycle = db.node.execute(&format!(
            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 1) {{lifecycle_state}}}}"#,
            escape_graphql_string(&pending_doc)
        ))
        .await;
        assert_eq!(
            lifecycle.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
            "interrupted"
        );
    }
    let pending_rows = db.node.execute(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}, purpose: {{_eq: "normal"}}, lifecycle_state: {{_eq: "pending"}}}}, limit: 2) {{request_id}}}}"#,
        escape_graphql_string(NODE_DID),
        escape_graphql_string(&stopped_session)
    )).await;
    assert_eq!(
        pending_rows.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        stop_case["pending_empty"].as_bool().unwrap()
    );
    gents::goal::set_goal(
        &db.node,
        NODE_DID,
        &stopped_session,
        None,
        Some(gents::goal::GoalStatus::Complete),
        None,
    )
    .await
    .unwrap();
    gents::session::resume_session(&db.node, NODE_DID, &stopped_session, None)
        .await
        .unwrap();
    let resumed_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(NODE_DID),
        escape_graphql_string(&stopped_session)
    )).await;
    assert_eq!(
        resumed_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        resume_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        gents::goal::load_canonical_goal(&db.node, NODE_DID, &stopped_session)
            .await
            .unwrap()
            .unwrap()
            .status,
        resume_case["goal_status"].as_str().unwrap()
    );
    db.node.shutdown().await;
}
