use super::*;

#[tokio::test]
async fn opening_request_is_the_agent_new_call_not_a_same_second_message() {
    let (_tempdir, node, node_did) = test_node().await;
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let node_literal = escape_graphql_string(&node_did);
    let agent_literal = escape_graphql_string(&agent_id);
    let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
        request_id:"open-root", purpose:"normal", session_id:"session-1", node_did:"{node_literal}", requester_did:"{node_literal}", agent_id:"{agent_literal}", lifecycle_state:"processing"
    }}) {{_docID}} }}"#)).await;
    ensure_no_errors(&response, "seed opening parent").unwrap();
    let parent_doc = gents_protocol::graphql::extract_mutation_doc_id(
        &json!({"data":response.data}),
        "AgentRequest",
    )
    .unwrap();
    let parent: gents_protocol::row::AgentRequestRow = serde_json::from_value(json!({"_docID":parent_doc,"request_id":"open-root","node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1"})).unwrap();
    let open_call = seed_tool_call(&node, &parent, "call-open", "agent_new", "running", "").await;
    let message_call = seed_tool_call(
        &node,
        &parent,
        "call-message",
        "agent_message",
        "running",
        "",
    )
    .await;
    let mut opening_doc = None;
    for (request_id, call_id, call_doc) in [
        ("zz-opening", "call-open", &open_call),
        ("aa-message", "call-message", &message_call),
    ] {
        let request_id = escape_graphql_string(request_id);
        let parent = escape_graphql_string(&parent_doc);
        let call_id = escape_graphql_string(call_id);
        let call_doc = escape_graphql_string(call_doc);
        let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
            request_id:"{request_id}", purpose:"normal", session_id:"opened", node_did:"{node_literal}", requester_did:"{node_literal}", agent_id:"{agent_literal}", lifecycle_state:"processing", created_at:"2026-09-27T00:00:00Z",
            caused_by_parent_request_id:"open-root", caused_by_parent_request_doc_id:"{parent}", caused_by_parent_tool_call_id:"{call_id}", caused_by_parent_tool_call_doc_id:"{call_doc}"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "seed opened session request").unwrap();
        if request_id == "zz-opening" {
            opening_doc = Some(
                gents_protocol::graphql::extract_mutation_doc_id(
                    &json!({"data":response.data}),
                    "AgentRequest",
                )
                .unwrap(),
            );
        }
    }
    // The session records its opener: `(created_at, _docID)` alone would
    // tie these two requests on an unfixed document id.
    crate::caused_sessions::fixtures::create_started_session(
        &node,
        gents_protocol::session::AgentSession {
            agent_id: agent_id.clone(),
            ..crate::caused_sessions::fixtures::fixture_session(&node_did, "opened")
        },
        gents_protocol::session::SessionReference {
            node_did: node_did.clone(),
            session_id: "session-1".into(),
            requester_did: Some(node_did.clone()),
        },
        &parent_doc,
        opening_doc.as_deref().unwrap(),
    )
    .await;
    let started = crate::caused_sessions::load_direct_caused_sessions(&node, &parent)
        .await
        .unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].scope.session_id, "opened");
    assert_eq!(started[0].first.request_id, "zz-opening");
    assert_eq!(
        started[0]
            .first
            .caused_by_parent_tool_call_doc_id
            .as_deref(),
        Some(open_call.as_str())
    );
}

#[tokio::test]
async fn child_pane_streams_followups_before_finish_and_excludes_foreign_requester() {
    let (_tempdir, node, node_did) = test_node().await;
    let manager = TurnManager::new(node.clone(), test_config(String::new(), &node_did));
    let engine = test_engine(node.clone());
    let (buffer, sender) = buffer_sender();
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
        request_id:"pane-root", purpose:"normal", session_id:"session-1", node_did:"{node_did}", requester_did:"{node_did}", agent_id:"{agent_id}", lifecycle_state:"processing"
    }}) {{_docID}} }}"#)).await;
    ensure_no_errors(&response, "seed pane root").unwrap();
    let doc = gents_protocol::graphql::extract_mutation_doc_id(
        &json!({"data":response.data}),
        "AgentRequest",
    )
    .unwrap();
    let parent: gents_protocol::row::AgentRequestRow = serde_json::from_value(json!({"_docID":doc,"request_id":"pane-root","node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1"})).unwrap();
    let tool = seed_tool_call(&node, &parent, "call-1", "agent_new", "running", "").await;
    let child = seed_child_request(&node, &parent, &tool, "pane-child", "processing").await;
    let mut followup = None;
    for (id, requester, text) in [
        (
            "pane-child",
            Some(node_did.as_str()),
            "Original child output",
        ),
        (
            "pane-followup",
            Some(node_did.as_str()),
            "Steered child output",
        ),
        ("pane-foreign", Some("did:foreign"), "MUST NOT LEAK"),
    ] {
        let row = if id == "pane-child" {
            child.clone()
        } else {
            let requester_field = requester
                .map(|did| format!("\"{}\"", escape_graphql_string(did)))
                .unwrap_or_else(|| "null".into());
            let created_at = chrono::Utc::now().to_rfc3339();
            let response = node.execute(&format!(
                r#"mutation {{create_AgentRequest(input: {{request_id:"{}", purpose:"normal", session_id:"session-1-child", node_did:"{}", requester_did:{requester_field}, agent_id:"{}", lifecycle_state:"processing", created_at:"{}"}}) {{_docID}} }}"#,
                escape_graphql_string(id),
                escape_graphql_string(&node_did),
                escape_graphql_string(&agent_id),
                escape_graphql_string(&created_at),
            )).await;
            ensure_no_errors(&response, "seed child followup").unwrap();
            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                &json!({"data":response.data}),
                "AgentRequest",
            )
            .unwrap();
            serde_json::from_value(json!({"_docID":doc,"request_id":id,"node_did":node_did,"requester_did":requester,"agent_id":agent_id,"session_id":"session-1-child"})).unwrap()
        };
        let sequence = if id == "pane-followup" { 2 } else { 1 };
        seed_assistant_message(&node, &row, sequence, text).await;
        if id == "pane-followup" {
            followup = Some(row);
        }
    }
    let spawn_doc = crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
        &node,
        &child,
        "child-process-meta",
        "spawn_process",
        "completed",
        r#"{"tool_name":"bash","args":{"command":"echo CHILD_BG_OUTPUT"}}"#,
        Some("spawned child-bash"),
        None,
        None,
        None,
    )
    .await;
    let child_tool_doc = crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
        &node,
        &child,
        "child-bash",
        "bash",
        "running",
        r#"{"command":"echo CHILD_BG_OUTPUT"}"#,
        None,
        Some(&spawn_doc),
        None,
        None,
    )
    .await;
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, input: {{await_mode: "background"}}) {{_docID}} }}"#,
            escape_graphql_string(&child_tool_doc)
        ))
        .await;
    ensure_no_errors(&response, "mark child tool backgrounded").unwrap();
    crate::commands::grok_shim::test_fixtures::seed_canonical_live_tool_output(
        node.as_ref(),
        &child,
        &child_tool_doc,
        "CHILD_BG_OUTPUT",
    )
    .await;
    seed_assistant_message(&node, &parent, 7, "PARENT MUST WAIT").await;
    let cursor = Mutex::new(RequestCursor::new());
    cursor.lock().await.request = Some(parent);
    let mut timing = RequestUpdateTiming::new(0);
    manager
        .stream_projection_updates_mode(
            "session-1",
            "pane-root",
            Some("parent-prompt"),
            &sender,
            &engine,
            &cursor,
            &mut timing,
            false,
            0,
            true,
        )
        .await
        .unwrap();
    let activity = parse_buffered_updates(&buffer).await;
    let task_start = activity
        .iter()
        .position(|row| {
            row["params"]["update"]["sessionUpdate"] == "task_backgrounded"
                && row["params"]["sessionId"] == "session-1-child"
        })
        .unwrap();
    let output = activity
        .iter()
        .position(|row| {
            row["params"]["update"]["sessionUpdate"] == "monitor_event"
                && row["params"]["sessionId"] == "session-1-child"
        })
        .unwrap();
    assert!(task_start < output);
    assert_eq!(
        activity[output]["params"]["update"]["event_text"],
        "CHILD_BG_OUTPUT"
    );
    assert!(activity
        .iter()
        .any(|row| row["params"]["sessionId"] == "session-1-child"
            && row["params"]["update"]["toolCallId"] == "child-bash"));
    assert!(!activity.iter().any(|row| {
        row.pointer("/params/update/content/text")
            .and_then(Value::as_str)
            == Some("PARENT MUST WAIT")
    }));
    for row in activity
        .iter()
        .filter(|row| row["params"]["sessionId"] == "session-1")
    {
        assert!(row.pointer("/params/_meta/promptId").is_none());
        assert!(row.pointer("/params/_meta/turnStartMs").is_none());
    }
    complete_child_request(&node, &child).await;
    crate::commands::grok_shim::test_fixtures::complete_canonical_spawned_process_output(
        node.as_ref(),
        &child,
        &child_tool_doc,
        "CHILD_BG_OUTPUT",
    )
    .await;
    complete_tool_call(&node, &tool, "done").await;
    manager
        .stream_projection_updates(
            "session-1",
            "pane-root",
            None,
            &sender,
            &engine,
            &cursor,
            &mut timing,
            true,
            0,
        )
        .await
        .unwrap();
    assert!(!buffered_update_kinds(&buffer)
        .await
        .iter()
        .any(|kind| kind == "subagent_finished"));
    complete_child_request(&node, followup.as_ref().unwrap()).await;
    manager
        .stream_projection_updates(
            "session-1",
            "pane-root",
            None,
            &sender,
            &engine,
            &cursor,
            &mut timing,
            true,
            0,
        )
        .await
        .unwrap();
    let updates = parse_buffered_updates(&buffer).await;
    let position = |kind: &str| {
        updates
            .iter()
            .position(|row| row["params"]["update"]["sessionUpdate"] == kind)
            .unwrap()
    };
    let spawned = position("subagent_spawned");
    let finished = position("subagent_finished");
    let child_tools: Vec<_> = updates
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row["params"]["sessionId"] == "session-1-child"
                && row["params"]["update"]["toolCallId"] == "child-bash"
        })
        .collect();
    assert!(
        !child_tools.is_empty(),
        "child pane receives native tool frames"
    );
    assert!(child_tools
        .iter()
        .all(|(index, _)| spawned < *index && *index < finished));
    for text in ["Original child output", "Steered child output"] {
        let matches: Vec<_> = updates
            .iter()
            .enumerate()
            .filter(|(_, row)| row["params"]["update"]["content"]["text"] == text)
            .collect();
        assert_eq!(matches.len(), 1, "child cursor must not replay {text}");
        assert!(spawned < matches[0].0 && matches[0].0 < finished);
        assert_eq!(matches[0].1["params"]["sessionId"], "session-1-child");
    }
    assert!(!updates
        .iter()
        .any(|row| row.to_string().contains("MUST NOT LEAK")));
}
