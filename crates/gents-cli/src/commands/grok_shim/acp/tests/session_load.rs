use super::*;
use crate::commands::grok_shim::acp::session_document::ensure_session_document;

#[tokio::test]
async fn stalled_replay_reserves_only_its_session_and_abort_releases_it() {
    let (_dir, service) = test_service().await;
    let service = Arc::new(service);
    ensure_session_document(&service.config, "stalled-history")
        .await
        .unwrap();
    let seeded = service
        .config
        .node
        .execute(
            r#"mutation {create_AgentRequest(input:{purpose: "normal", 
            request_id:"stalled-request", session_id:"stalled-history", node_did:"did:test:grok-shim",
            requester_did:"did:test:grok-shim", content:"Original prompt", lifecycle_state:"completed",
            created_at:"2026-09-01T12:00:00Z"
        }) {_docID}}"#,
        )
        .await;
    ensure_no_errors(&seeded, "seed stalled replay").unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let hold_output = buffer.lock().await;
    let sender = PromptSender::Buffer {
        buffer: buffer.clone(),
    };
    let request = AcpRequest::from_payload(&request_payload(
        "session/load",
        json!({"sessionId":"stalled-history"}),
    ))
    .unwrap();
    let task = tokio::spawn({
        let service = service.clone();
        let sender = sender.clone();
        let request = request.clone();
        async move { service.handle_session_load(&request, &sender).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !service
            .loading_sessions
            .lock()
            .unwrap()
            .contains("stalled-history")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("load reservation");
    let unrelated = AcpRequest::from_payload(&request_payload(
        "session/new",
        json!({"_meta":{"sessionId":"unrelated"}}),
    ))
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        service.handle_session_new(&unrelated),
    )
    .await
    .expect("stalled replay must not block another session")
    .unwrap();
    assert!(service
        .handle_session_load(&request, &sender)
        .await
        .is_err());
    let conflict = AcpRequest::from_payload(&request_payload(
        "session/new",
        json!({"_meta":{"sessionId":"stalled-history"}}),
    ))
    .unwrap();
    assert!(service.handle_session_new(&conflict).await.is_err());
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert!(service.loading_sessions.lock().unwrap().is_empty());
    drop(hold_output);
    service
        .handle_session_load(&request, &sender)
        .await
        .unwrap();
    assert!(service.loading_sessions.lock().unwrap().is_empty());
    assert!(service
        .sessions
        .lock()
        .await
        .contains_key("stalled-history"));
}

#[tokio::test]
async fn session_load_replays_all_message_pages_without_creating_requests() {
    let (_dir, service) = test_service().await;
    ensure_session_document(&service.config, "resume-history")
        .await
        .unwrap();
    let node = &service.config.node;
    let result = node
        .execute(
            r#"mutation { create_AgentRequest(input: {purpose: "normal", 
            request_id: "resume-request", session_id: "resume-history", node_did: "did:test:grok-shim",
            requester_did: "did:test:grok-shim", content: "Original human prompt",
            lifecycle_state: "completed", created_at: "2026-09-01T12:00:00Z"
        }) { _docID } }"#,
        )
        .await;
    ensure_no_errors(&result, "seed resume request").unwrap();
    let request = node
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "resume-request"}}, limit: 2) {_docID request_id node_did requester_did session_id} }"#)
        .await;
    ensure_no_errors(&request, "lookup resume request").unwrap();
    let rows = request.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1, "resume request identity must be exact");
    let request: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows[0].clone()).unwrap();
    for sequence in 1_i64..=70 {
        let message_key = gents::session::sequence_message_key(
            "did:test:grok-shim",
            "resume-history",
            Some("did:test:grok-shim"),
            u32::try_from(sequence).expect("fixture sequence fits u32"),
        );
        seed_canonical_assistant_message(
            node,
            &request,
            &message_key,
            sequence,
            "",
            &format!("REPLAY_{sequence:03}\n"),
        )
        .await;
    }
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/load",
            json!({"sessionId":"resume-history", "cwd":"/tmp", "mcpServers":[]}),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().unwrap());
    assert!(response.get("error").is_none(), "{response}");
    assert!(response["result"]["_meta"]["x.ai/runningPromptId"].is_null());
    let events: Vec<_> = dispatch
        .notifications
        .iter()
        .map(|line| parse_response(line))
        .filter(|line| line["method"] == "session/update")
        .collect();
    assert!(events
        .iter()
        .all(|event| event["params"]["_meta"]["isReplay"] == true));
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        texts
            .iter()
            .filter(|text| **text == "Original human prompt")
            .count(),
        1
    );
    let assistant = texts
        .iter()
        .filter(|text| text.starts_with("REPLAY_"))
        .copied()
        .collect::<String>();
    assert_eq!(
        assistant,
        (1..=70)
            .map(|sequence| format!("REPLAY_{sequence:03}\n"))
            .collect::<String>()
    );
    let result = node
        .execute(r#"{ AgentRequest { request_id } AgentSession { session_id requester_did } }"#)
        .await;
    ensure_no_errors(&result, "resume write check").unwrap();
    assert_eq!(
        result.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result.data.as_ref().unwrap()["AgentSession"][0]["requester_did"],
        "did:test:grok-shim"
    );
    let repeated = service
        .handle_acp_payload(&request_payload(
            "session/load",
            json!({"sessionId":"resume-history"}),
        ))
        .await;
    assert_eq!(
        parse_response(repeated.response.as_deref().unwrap())["error"]["code"],
        -32602
    );
    service.turns.handle_disconnect().await.unwrap();
}

#[tokio::test]
async fn history_picker_scopes_searches_paginates_and_preserves_legacy_sessions() {
    let (_dir, service) = test_service().await;
    for (id, requester, provenance) in [
        ("older", "null", ""),
        ("newer", "\"did:test:grok-shim\"", ""),
        ("foreign-session", "\"did:test:foreign\"", ""),
        (
            "child",
            "\"did:test:grok-shim\"",
            r#"provenance: {parent_request_doc_id: "doc-a"},"#,
        ),
    ] {
        let result = service
            .config
            .node
            .execute(&format!(
                r#"mutation {{ create_AgentSession(input: {{
                session_id: "{id}", node_did: "did:test:grok-shim", requester_did: {requester},
                {provenance} agent_id: "did:test:grok-shim:default", created_at: "2026-09-01T00:00:00Z"
            }}) {{ _docID }} }}"#
            ))
            .await;
        ensure_no_errors(&result, "seed history owner").unwrap();
    }
    for (id, session, requester, content, day, parent) in [
        (
            "a",
            "older",
            "did:test:grok-shim",
            "First older prompt",
            1,
            "null",
        ),
        (
            "b",
            "newer",
            "did:test:grok-shim",
            "First newer prompt",
            2,
            "null",
        ),
        (
            "c",
            "foreign-session",
            "did:test:grok-shim",
            "SECRET session",
            4,
            "null",
        ),
        (
            "d",
            "older",
            "did:test:foreign",
            "SECRET request",
            5,
            "null",
        ),
        (
            "e",
            "child",
            "did:test:grok-shim",
            "Child instructions",
            6,
            "\"a\"",
        ),
        (
            "f",
            "older",
            "did:test:grok-shim",
            "Later searchable NEEDLE",
            3,
            "null",
        ),
    ] {
        let result = service
            .config
            .node
            .execute(&format!(
                r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", 
                request_id: "{id}", session_id: "{session}", node_did: "did:test:grok-shim",
                requester_did: "{requester}", content: "{content}", lifecycle_state: "completed",
                caused_by_parent_request_id: {parent}, created_at: "2026-09-0{day}T12:00:00Z"
            }}) {{ _docID }} }}"#
            ))
            .await;
        ensure_no_errors(&result, "seed history request").unwrap();
    }
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "_x.ai/session/list",
            json!({"limit":1, "cwd":"/not-historical"}),
        ))
        .await;
    let first = parse_response(dispatch.response.as_deref().unwrap());
    assert!(first.get("error").is_none(), "{first}");
    let page = &first["result"];
    assert_eq!(page["sessions"][0]["sessionId"], "older");
    assert_eq!(page["sessions"][0]["summary"], "First older prompt");
    assert!(page["sessions"][0].get("cwd").is_none());
    assert!(page["sessions"][0].get("modelId").is_none());
    assert_eq!(page["_meta"]["x.ai/listScope"], "all");
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "x.ai/session/list",
            json!({"limit":1, "cursor":page["nextCursor"]}),
        ))
        .await;
    let second = parse_response(dispatch.response.as_deref().unwrap());
    assert_eq!(second["result"]["sessions"][0]["sessionId"], "newer");
    assert!(second["result"]["nextCursor"].is_null());
    for (query, count) in [("needle", 1), ("secret", 0)] {
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "x.ai/session/list",
                json!({"query":query}),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert_eq!(
            response["result"]["sessions"].as_array().unwrap().len(),
            count
        );
    }
    let rows = crate::commands::grok_shim::sessions::load(
        &service.config.node,
        &service.config.node_did,
        &service.config.agent_id,
        "older",
    )
    .await
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "f"]
    );
    assert!(crate::commands::grok_shim::sessions::load(
        &service.config.node,
        &service.config.node_did,
        &service.config.agent_id,
        "foreign-session"
    )
    .await
    .is_err());
}
