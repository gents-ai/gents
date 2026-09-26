use super::*;

const GHOST_BEHAVIOR_ID: &str = "r4-ghost-child";

async fn setup_ghost_behavior_fixture(test_name: &str) -> SpawnFixture {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();

    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        GHOST_BEHAVIOR_ID,
        "r4-ghost-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "r4-parent-tools",
        vec![subagent_target(
            &agent_did,
            GHOST_BEHAVIOR_ID,
            &agent_did,
            GHOST_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;

    let source = spawn_subagent_source(
        db.node.clone(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        PARENT_BEHAVIOR_ID,
    );

    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-parent");
    let parent_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    create_parent_request(
        db.node.as_ref(),
        &agent_did,
        &request_id,
        &session_id,
        0,
        parent_deadline,
    )
    .await;
    crate::support::create_agent_session_in_scope(
        db.node.as_ref(),
        &agent_did,
        &session_id,
        PARENT_BEHAVIOR_ID,
        "2026-05-13T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        &agent_did,
        None,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(parent_deadline)).await;

    SpawnFixture {
        db,
        hook,
        session_id,
        request_id,
        parent_subagent_depth: 0,
        extra_parent_fields: String::new(),
        operator_tool_root: None,
        parent_deadline,
        agent_did,
        legacy_source: std::sync::Mutex::new(Some(source)),
    }
}

async fn run_ghost_target_turn(fixture: &SpawnFixture, tool_call_id: &str, args: &str) {
    const MARKER: &str = "ghost parent prompt";
    let backend = MockStreamingBackend::start_with_plans(
        "r4-ghost-model",
        vec![StreamPlan::new(
            MARKER,
            vec![
                StreamResponse::Stream(StreamScript::paused_before(
                    MARKER,
                    vec![StreamChunk::tool_call(tool_call_id, "spawn_subagent", args)],
                )),
                StreamResponse::completes(MARKER, ["parent complete"]),
            ],
        )],
    )
    .expect("start ghost backend");
    bind_behavior_backend(
        fixture.db.node.as_ref(),
        &fixture.agent_did,
        PARENT_BEHAVIOR_ID,
        "r4-ghost-backend",
        backend.endpoint(),
        "r4-ghost-model",
    )
    .await;
    let request_id = escape_graphql_string(&fixture.request_id);
    for mutation in [
        format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ delete_AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.session_id)
        ),
    ] {
        let response = fixture.db.node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "replace ghost fixture: {:?}",
            response.errors
        );
    }
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut session =
        crate::support::session_document(&fixture.session_id, PARENT_BEHAVIOR_ID, &now);
    session.agent_did = fixture.agent_did.clone();
    session.requester_did = Some(fixture.agent_did.clone());
    session.title = Some(gents_protocol::session::SessionTitle {
        text: "Ghost target validation".to_string(),
        source: gents_protocol::session::SessionTitleSource::Generated,
    });
    crate::support::create_session_document(fixture.db.node.as_ref(), &session).await;
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &fixture.request_id,
        &fixture.agent_did,
        &fixture.agent_did,
        PARENT_BEHAVIOR_ID,
        &fixture.session_id,
        MARKER,
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            &fixture.agent_did,
        ),
    );
    request.valid_until = Some(valid_until);
    gents::sign_agent_request_create(fixture.db.node_identity.as_ref(), &mut request)
        .await
        .expect("sign ghost request");
    let response = fixture
        .db
        .node
        .execute(&request.graphql_mutation().unwrap())
        .await;
    assert!(
        !response.has_errors(),
        "create ghost request: {:?}",
        response.errors
    );
    let identity: std::sync::Arc<dyn AgentIdentity> = fixture.db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
        fixture.db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build runtime while ghost target still exists");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(fixture.db.node.as_ref(), &agent_did).await;
    let runtime = BootedAgent::new(shutdown_tx, handle, agent_did);
    let observed_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while backend.observed_requests(MARKER) == 0 {
        assert!(
            tokio::time::Instant::now() < observed_deadline,
            "provider request never reached ghost pre-chunk gate"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let response = fixture
        .db
        .node
        .execute(&format!(
            r#"mutation {{ delete_AgentBehavior(filter: {{ agent_did: {{ _eq: "{}" }}, behavior_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.agent_did),
            escape_graphql_string(GHOST_BEHAVIOR_ID)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "delete ghost behavior: {:?}",
        response.errors
    );
    backend.release(MARKER);
    wait_for_parent_terminal(fixture, "completed").await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn spawn_subagent_rejects_local_target_whose_behavior_was_deleted() {
    let fixture = setup_ghost_behavior_fixture("spawn_subagent_ghost_behavior").await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": GHOST_BEHAVIOR_ID,
        "prompt": "should not spawn orphan",
        "await_mode": "background"
    })
    .to_string();

    run_ghost_target_turn(&fixture, "internal-spawn-ghost", &args).await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-ghost").await;
    let error = persisted_tool_result_json(&tool);
    assert_eq!(error["ok"], false, "spawn must be rejected");
    assert_eq!(
        error["failure_class"], "tool_not_allowed",
        "failure_class must be tool_not_allowed"
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap_or("")
            .contains("no longer exists"),
        "message must mention the behavior no longer exists"
    );

    assert_eq!(
        tool.lifecycle_state.as_deref(),
        Some("failed"),
        "tool call must be in failed state"
    );
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable"),
        "failure class must be serviceUnavailable"
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), &session_id, "internal-spawn-ghost")
            .await
            .is_none(),
        "must not write an orphan child AgentRequest"
    );
}

#[tokio::test]
async fn spawn_subagent_skip_payload_is_persisted_to_transcript() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_skip_transcript",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "child prompt for transcript",
        "await_mode": "background",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    run_canonical_spawn_turn(&fixture, "model-call-transcript", &args).await;

    let history = load_history(
        db.node.as_ref(),
        &session_id,
        db.node_identity.did(),
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();
    assert!(history.iter().any(|message| {
        matches!(
            message,
            Message::User { content }
                if matches!(content.first().expect("non-empty content"), UserContent::ToolResult(tool_result)
                    if matches!(tool_result.content.first().expect("non-empty content"), ToolResultContent::Text(Text { text })
                        if text.contains("child_request_id")
                            && text.contains("\"await_mode\": \"background\"")))
        )
    }));
}

#[tokio::test]
async fn spawn_subagent_rejects_unauthorized_target_without_child_request() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_unauthorized",
        vec!["different-child"],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    run_canonical_spawn_turn(&fixture, "internal-spawn-denied", &args).await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-denied").await;
    let error = persisted_tool_result_json(&tool);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], CHILD_BEHAVIOR_ID);

    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(tool
        .result
        .as_deref()
        .is_some_and(|result| result.contains("\"tool_not_allowed\"")));
    assert!(
        child_request_for_tool(db.node.as_ref(), &session_id, "internal-spawn-denied")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_when_spawn_disabled_without_child_request() {
    let fixture = setup_spawn_fixture_with_flags(
        "spawn_subagent_spawn_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
        true,
    )
    .await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    run_canonical_spawn_turn(&fixture, "internal-spawn-disabled", &args).await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-disabled").await;
    let error = persisted_tool_result_json(&tool);
    assert_eq!(error["failure_class"], "tool_not_allowed");

    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), &session_id, "internal-spawn-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_background_when_background_disabled_without_child_request() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_background_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
    )
    .await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn in background",
        "await_mode": "background"
    })
    .to_string();

    run_canonical_spawn_turn(&fixture, "internal-spawn-bg-disabled", &args).await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-bg-disabled").await;
    let error = persisted_tool_result_json(&tool);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], "background");

    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), &session_id, "internal-spawn-bg-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_deadline_after_parent_without_child_request() {
    let fixture =
        setup_spawn_fixture("spawn_subagent_deadline", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "deadline too late",
        "await_mode": "background",
        "deadline": (parent_deadline + chrono::Duration::days(1)).to_rfc3339()
    })
    .to_string();

    run_canonical_spawn_turn(&fixture, "internal-spawn-deadline", &args).await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-deadline").await;
    let error = persisted_tool_result_json(&tool);
    assert_eq!(
        error["failure_class"], "invalid_tool_arguments",
        "unexpected deadline rejection payload: {error}"
    );
    assert_eq!(error["path"], "/deadline");

    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("argumentInvalid"));
    assert!(
        child_request_for_tool(db.node.as_ref(), &session_id, "internal-spawn-deadline")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_depth_ceiling_without_child_request() {
    let fixture =
        setup_spawn_fixture("spawn_subagent_depth", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    configure_subagent_behavior(
        fixture.db.node.as_ref(),
        &fixture.agent_did,
        CHILD_BEHAVIOR_ID,
        "r4-child-tools",
        vec![subagent_target(
            &fixture.agent_did,
            CHILD_BEHAVIOR_ID,
            &fixture.agent_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;
    drop(
        fixture
            .legacy_source
            .lock()
            .expect("legacy source mutex")
            .take(),
    );
    for (collection, key, value) in [
        ("AgentRequest", "request_id", fixture.request_id.as_str()),
        ("AgentSession", "session_id", fixture.session_id.as_str()),
    ] {
        let value = escape_graphql_string(value);
        let response = fixture.db.node.execute(&format!(
            "mutation {{ delete_{collection}(filter: {{ {key}: {{ _eq: \"{value}\" }} }}) {{ _docID }} }}"
        )).await;
        assert!(
            !response.has_errors(),
            "replace depth root {collection}: {:?}",
            response.errors
        );
    }
    let mut child_plans = Vec::new();
    for depth in 1..=MAX_SUBAGENT_DEPTH {
        let prompt = format!("real depth {depth}");
        let next_prompt = format!("real depth {}", depth + 1);
        let call_id = if depth == MAX_SUBAGENT_DEPTH {
            "depth-overflow".to_string()
        } else {
            format!("depth-spawn-{depth}")
        };
        child_plans.push(StreamPlan::current_authored_user(
            prompt.clone(),
            vec![
                StreamResponse::streams(
                    &prompt,
                    vec![StreamChunk::tool_call(
                        call_id,
                        "spawn_subagent",
                        json!({"name": CHILD_BEHAVIOR_ID, "prompt": next_prompt, "await_mode": "background"}).to_string(),
                    )],
                ),
                StreamResponse::completes(&prompt, ["depth turn complete"]),
            ],
        ));
    }
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let runtime = boot_accepted_turn(
        &fixture.db,
        AcceptedTurnSpec {
            backend_id: "r4-depth-backend",
            model: "r4-depth-model",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id: &fixture.request_id,
            session_id: &fixture.session_id,
            prompt: "real depth root",
            accepted_chunks: vec![StreamChunk::tool_call(
                "depth-spawn-root",
                "spawn_subagent",
                json!({"name": CHILD_BEHAVIOR_ID, "prompt": "real depth 1", "await_mode": "background"}).to_string(),
            )],
            child_plans,
            valid_until: Some(&valid_until),
            subagent_depth: None,
            request_setup: None,
        },
        DocumentRuntimeOptions { tool_ceiling: ToolCeiling::meta_only(), ..Default::default() },
    ).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (boundary_request_id, boundary_session_id) = loop {
        let response = fixture.db.node.execute(&format!(
            "{{ AgentRequest(filter: {{ agent_did: {{ _eq: \"{}\" }}, purpose: {{ _eq: \"normal\" }} }}, limit: {}) {{ _docID request_id session_id lifecycle_state failure_reason subagent_depth caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id }} }}",
            escape_graphql_string(&fixture.agent_did),
            (MAX_SUBAGENT_DEPTH + 1) * 3,
        )).await;
        assert!(
            !response.has_errors(),
            "query real depth ancestry: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array());
        let ancestry_diagnostic = rows
            .map(|rows| serde_json::to_string(rows).expect("serialize ancestry diagnostic"))
            .unwrap_or_else(|| format!("missing rows; response={response:?}"));
        let complete_chain = rows.is_some_and(|rows| {
            (0..=MAX_SUBAGENT_DEPTH).all(|depth| {
                rows.iter()
                    .filter(|row| {
                        row["subagent_depth"].as_u64() == Some(depth as u64)
                            && (row["request_id"].as_str() == Some(fixture.request_id.as_str())
                                || row["caused_by_parent_tool_call_doc_id"]
                                    .as_str()
                                    .is_some_and(|id| !id.is_empty()))
                    })
                    .count()
                    == 1
            })
        });
        if complete_chain {
            let rows = rows.expect("complete ancestry rows");
            let mut by_depth = rows
                .iter()
                .filter(|row| {
                    row["request_id"].as_str() == Some(fixture.request_id.as_str())
                        || row["caused_by_parent_tool_call_doc_id"]
                            .as_str()
                            .is_some_and(|id| !id.is_empty())
                })
                .collect::<Vec<_>>();
            by_depth.sort_by_key(|row| row["subagent_depth"].as_u64());
            assert_eq!(
                by_depth.len(),
                MAX_SUBAGENT_DEPTH as usize + 1,
                "real ancestry must contain exactly the root and one request at every allowed depth"
            );
            for (depth, pair) in by_depth.windows(2).enumerate() {
                let parent = pair[0];
                let child = pair[1];
                assert_eq!(child["subagent_depth"].as_u64(), Some((depth + 1) as u64));
                assert_eq!(
                    child["caused_by_parent_request_id"].as_str(),
                    parent["request_id"].as_str(),
                    "depth {} must bind to the preceding physical request id",
                    depth + 1
                );
                assert_eq!(
                    child["caused_by_parent_request_doc_id"].as_str(),
                    parent["_docID"].as_str(),
                    "depth {} must bind to the preceding physical request document",
                    depth + 1
                );
                assert!(
                    child["caused_by_parent_tool_call_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "depth {} must carry its physical parent tool-call id",
                    depth + 1
                );
                assert!(
                    child["caused_by_parent_tool_call_doc_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "depth {} must carry its physical parent tool-call document",
                    depth + 1
                );
            }
            let boundary = by_depth[MAX_SUBAGENT_DEPTH as usize];
            break (
                boundary["request_id"]
                    .as_str()
                    .expect("boundary request id")
                    .to_string(),
                boundary["session_id"]
                    .as_str()
                    .expect("boundary session id")
                    .to_string(),
            );
        }
        if tokio::time::Instant::now() >= deadline {
            let tools = fixture.db.node.execute(
                "{ AgentToolCall(filter: { tool_name: { _eq: \"spawn_subagent\" } }) { _docID request_id request_doc_id session_id tool_call_id lifecycle_state tool_failure_class child_request_id spawn_target_did spawn_behavior_id await_mode cancel_policy } }",
            ).await;
            panic!(
                "timed out building real depth ancestry; rows={ancestry_diagnostic}; tools={:?}; provider calls root={} depth1={} depth2={} depth3={}; provider bodies={:?}",
                tools.data,
                runtime.backend.observed_requests("real depth root"),
                runtime.backend.observed_requests("real depth 1"),
                runtime.backend.observed_requests("real depth 2"),
                runtime.backend.observed_requests("real depth 3"),
                runtime.backend.observed_completion_bodies(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let escaped_boundary = escape_graphql_string(&boundary_request_id);
    loop {
        let response = fixture.db.node.execute(&format!(
            "{{ AgentToolCall(filter: {{ request_id: {{ _eq: \"{escaped_boundary}\" }}, lifecycle_state: {{ _eq: \"failed\" }} }}, limit: 1) {{ _docID }} }}"
        )).await;
        if response
            .data
            .as_ref()
            .and_then(|data| data["AgentToolCall"].as_array())
            .is_some_and(|rows| !rows.is_empty())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for depth rejection"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let error = canonical_tool_payload_json_for_scope(
        fixture.db.node.as_ref(),
        &boundary_session_id,
        &fixture.agent_did,
        Some(&fixture.agent_did),
        "depth-overflow",
    )
    .await;
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "invalid_tool_arguments");
    assert_eq!(error["code"], "subagent_depth_exceeded");
    assert_eq!(error["parent_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));
    assert_eq!(error["max_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));

    let response = fixture.db.node.execute(&format!(
        "{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: \"{escaped_boundary}\" }}, purpose: {{ _eq: \"normal\" }} }}, limit: 1) {{ _docID }} }}"
    )).await;
    let children = response
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
        .expect("overflow child rows");
    assert!(
        children.is_empty(),
        "depth rejection must not materialize a ghost child"
    );
    runtime.shutdown().await;
}
