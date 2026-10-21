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
    let node_did = db.node_identity.did();

    let interrupted_session = uuid::Uuid::new_v4().to_string();
    assert_eq!(interrupt_case["initial_pending_count"], 0);
    support::create_agent_session_in_scope(
        &db.node,
        node_did,
        &interrupted_session,
        AGENT_NAME,
        "2026-10-20T10:00:00Z",
    )
    .await;
    gents::goal::set_goal(
        &db.node,
        node_did,
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
                node_did: node_did.to_owned(),
                session_id: interrupted_session.clone(),
                requester_did: None,
            },
        )
        .await
        .unwrap(),
        None
    );
    let interrupted_goal =
        gents::goal::load_canonical_goal(&db.node, node_did, &interrupted_session)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        interrupted_goal.status,
        interrupt_case["goal_status"].as_str().unwrap()
    );
    let interrupted_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&interrupted_session)
    )).await;
    assert_eq!(
        interrupted_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        interrupt_case["stopped"].as_bool().unwrap()
    );

    let stopped_session = uuid::Uuid::new_v4().to_string();
    support::create_agent_session_in_scope(
        &db.node,
        node_did,
        &stopped_session,
        AGENT_NAME,
        "2026-10-20T10:00:01Z",
    )
    .await;
    let mut pending_ids = Vec::new();
    let mut pending_docs = Vec::new();
    for index in 0..stop_case["initial_pending_count"].as_u64().unwrap() {
        let pending_id = uuid::Uuid::new_v4().to_string();
        let pending_doc = support::create_request_for_agent_with_signed_fields(
            &db.node,
            node_did,
            &pending_id,
            &stopped_session,
            "pending",
            &format!("2026-10-20T10:00:{:02}Z", 2 + index),
            None,
            None,
            None,
            None,
        )
        .await;
        pending_ids.push(pending_id);
        pending_docs.push(pending_doc);
    }
    gents::goal::set_goal(
        &db.node,
        node_did,
        &stopped_session,
        Some("stop and later resume"),
        Some(gents::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let receipt = gents::session::stop_session(&db.node, node_did, &stopped_session, None)
        .await
        .unwrap();
    assert_eq!(receipt.drained_request_ids, pending_ids);
    let stopped_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session)
    )).await;
    assert_eq!(
        stopped_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        stop_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        gents::goal::load_canonical_goal(&db.node, node_did, &stopped_session)
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
        escape_graphql_string(node_did),
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
        node_did,
        &stopped_session,
        None,
        Some(gents::goal::GoalStatus::Complete),
        None,
    )
    .await
    .unwrap();
    gents::session::resume_session(&db.node, node_did, &stopped_session, None)
        .await
        .unwrap();
    let resumed_session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: null}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&stopped_session)
    )).await;
    assert_eq!(
        resumed_session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        resume_case["stopped"].as_bool().unwrap()
    );
    assert_eq!(
        gents::goal::load_canonical_goal(&db.node, node_did, &stopped_session)
            .await
            .unwrap()
            .unwrap()
            .status,
        resume_case["goal_status"].as_str().unwrap()
    );
    db.node.shutdown().await;
}

#[tokio::test]
async fn generated_resume_after_settlement_stages_the_paused_goal_continuation() {
    let resume_case = model_case("resume_after_settlement");
    let db = test_db("session-control-resume").await;
    let node_did = db.node_identity.did();
    let session_id = uuid::Uuid::new_v4().to_string();
    support::create_session_document(
        &db.node,
        &gents_protocol::session::AgentSession {
            requester_did: Some(node_did.to_owned()),
            ..support::session_document_in_scope(
                node_did,
                &session_id,
                AGENT_NAME,
                "2026-10-20T11:00:00Z",
            )
        },
    )
    .await;
    let predecessor_id = uuid::Uuid::new_v4().to_string();
    let mut predecessor = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &predecessor_id,
        node_did,
        node_did,
        AGENT_NAME,
        &session_id,
        "Begin durable work",
        "interactive",
        "2026-10-20T11:00:01Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(node_did),
    );
    gents::sign_agent_request_create_as_registered_target(&mut predecessor)
        .await
        .unwrap();
    let created = db
        .node
        .execute(&predecessor.graphql_mutation().unwrap())
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let predecessor_doc = support::exact_request_doc_id(&db.node, &predecessor_id).await;
    support::set_request_lifecycle_state(&db.node, &predecessor_doc, "processing").await;
    gents::goal::set_goal(
        &db.node,
        node_did,
        &session_id,
        Some("resume after the stopped turn settles"),
        Some(gents::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let receipt = gents::session::stop_session(&db.node, node_did, &session_id, Some(node_did))
        .await
        .unwrap();
    assert_eq!(
        receipt.interrupted_request_id.as_deref(),
        Some(predecessor_id.as_str())
    );

    assert_eq!(
        gents::session::resume_session(&db.node, node_did, &session_id, Some(node_did))
            .await
            .is_err(),
        resume_case["refused_while_active"].as_bool().unwrap(),
        "resume must wait for the interrupted request to settle"
    );

    support::set_request_lifecycle_state(&db.node, &predecessor_doc, "interrupted").await;
    gents::session::resume_session(&db.node, node_did, &session_id, Some(node_did))
        .await
        .unwrap();

    let session_row = db.node.execute(&format!(
        r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, limit: 2) {{stopped_at}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&session_id)
    )).await;
    assert_eq!(
        session_row.data.unwrap()["AgentSession"][0]["stopped_at"].is_string(),
        resume_case["stopped"].as_bool().unwrap()
    );
    let goal = gents::goal::load_canonical_goal(&db.node, node_did, &session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.status, resume_case["goal_status"].as_str().unwrap());
    assert_eq!(
        goal.last_continued_from_request_id.as_deref(),
        Some(predecessor_id.as_str())
    );
    let continuation = db.node.execute(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}, lifecycle_state: {{_eq: "pending"}}}}, limit: 2) {{request_id}}}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&session_id)
    )).await;
    assert_eq!(
        continuation.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "resume stages exactly one signed continuation"
    );
    db.node.shutdown().await;
}
