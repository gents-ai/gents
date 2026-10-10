use super::*;
use gents::config_client::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test]
async fn live_read_observes_exact_request_lifecycle_and_every_crdt_record_without_history() {
    let node = std::sync::Arc::new(defra_node::NodeBuilder::default().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    ConfigAccess::write_local(&node, "test.live_request", r#"mutation {
        create_AgentRequest(input: {request_id:"logical", purpose:"normal", node_did:"agent",
            requester_did:"reader", session_id:"session", lifecycle_state:"processing",
            execution_generation:"generation"}) { _docID }
        create_AgentOutputSegment(input: {node_did:"agent", session_id:"session",
            request_doc_id:"historic", source:{kind:"invalid"}, payload:"must not be decoded"}) { _docID }
    }"#).await.unwrap();
    let data = execute_local_graphql_query(
        &node,
        &format!("query {{ AgentRequest {{ {AGENT_REQUEST_FIELDS} }} }}"),
        "test request",
    )
    .await
    .unwrap();
    let request: AgentRequestRow = parse_query_rows::<AgentRequestRow>(&data, AGENT_REQUEST_NAME)
        .unwrap()
        .remove(0);
    let doc = escape_graphql_string(request.doc_id.as_deref().unwrap());
    let source = r#"{kind:"provider_turn", scope:"inference.0", turn_index:0, attempt:0}"#;
    let writer = r#"{kind:"request_execution", execution_generation:"generation"}"#;
    // Distinct physical records at one coordinate must survive the read. A
    // closure without an ordinal must also survive; an ordinal-tail query
    // would miss both the twin and closure arriving after a later flush.
    for (ordinal, text, requester) in [
        (2, "later", "reader"),
        (0, "first", "reader"),
        (0, "twin", "reader"),
        (1, "foreign", "other"),
    ] {
        let bytes = text.len();
        let text = escape_graphql_string(text);
        let requester = escape_graphql_string(requester);
        ConfigAccess::write_local(&node, "test.live_segment", &format!(r#"mutation {{
            create_AgentOutputSegment(input: {{node_did:"agent", requester_did:"{requester}",
                session_id:"session", request_doc_id:"{doc}", source:{source}, writer:{writer},
                ordinal:{ordinal}, runs:[{{stream:0, bytes:{bytes}, declaration:{{block_index:0, part_index:0,
                    payload:{{kind:"text"}}}}}}], payload:"{text}", created_at:"2026-10-07T12:00:00Z"}}) {{ _docID }}
        }}"#)).await.unwrap();
    }
    ConfigAccess::write_local(
        &node,
        "test.live_close",
        &format!(
            r#"mutation {{
        create_AgentOutputSegment(input: {{node_did:"agent", requester_did:"reader",
            session_id:"session", request_doc_id:"{doc}", source:{source}, writer:{writer},
            ordinal:null, runs:null, payload:"", close:{{kind:"retracted"}},
            created_at:"2026-10-07T12:00:01Z"}}) {{ _docID }}
    }}"#
        ),
    )
    .await
    .unwrap();
    let large_payload = escape_graphql_string(&"inert payload ".repeat(16_384));
    for inert_source in [
        r#"{kind:"tool_call", tool_call_doc_id:"tool"}"#,
        r#"{kind:"authored", key:"prompt"}"#,
        r#"{kind:"provider_turn", scope:"compaction.0", turn_index:0, attempt:0}"#,
    ] {
        ConfigAccess::write_local(
            &node,
            "test.inert_payload",
            &format!(
                r#"mutation {{
            create_AgentOutputSegment(input: {{node_did:"agent", requester_did:"reader",
                session_id:"session", request_doc_id:"{doc}", source:{inert_source},
                payload:"{large_payload}"}}) {{ _docID }}
        }}"#
            ),
        )
        .await
        .unwrap();
    }
    // Both the node and request indexes exceed DefraDB's 1024-entry
    // estimation cap. A broad-index tie must not scan unrelated history.
    const INERT_ROWS: usize = 1_030;
    for start in (0..INERT_ROWS).step_by(32) {
        let batch = (start..(start + 32).min(INERT_ROWS)).map(|i| {
            let key = escape_graphql_string(&format!("tool-{i}"));
            format!(r#"s{i}: create_AgentOutputSegment(input: {{node_did:"agent", requester_did:"reader",
                session_id:"session", request_doc_id:"{doc}", source:{{kind:"tool_call", tool_call_doc_id:"{key}"}},
                payload:"inert"}}) {{ _docID }}"#)
        }).collect::<Vec<_>>().join("\n");
        ConfigAccess::write_local(
            &node,
            "test.request_cardinality",
            &format!("mutation {{ {batch} }}"),
        )
        .await
        .unwrap();
    }
    let history = (0..32).map(|i| {
        let history_id = escape_graphql_string(&format!("history-{i}"));
        format!(r#"h{i}: create_AgentOutputSegment(input: {{
            node_did:"agent", requester_did:"reader", session_id:"session", request_doc_id:"{history_id}",
            source:{{kind:"invalid"}}, payload:"unrelated history"}}) {{ _docID }}
            m{i}: create_AgentMessage(input: {{message_key:"{history_id}",
                node_did:"agent", requester_did:"reader", session_id:"session", request_doc_id:"{history_id}",
                publication:{{kind:"invalid"}}, blocks:"must not be decoded"}}) {{ _docID }}
            r{i}: create_AgentRequest(input: {{request_id:"{history_id}", purpose:"normal",
                node_did:"agent", requester_did:"reader", session_id:"session", lifecycle_state:"completed"}}) {{ _docID }}"#)
    }).collect::<Vec<_>>().join("\n");
    ConfigAccess::write_local(
        &node,
        "test.live_history",
        &format!("mutation {{ {history} }}"),
    )
    .await
    .unwrap();
    let query = tip_query(&request, true).unwrap();
    let plan = execute_local_graphql_query(
        &node,
        &query.replacen(
            "query DesktopSessionTip",
            "query DesktopSessionTip @explain(type: execute)",
            1,
        ),
        "explain live query",
    )
    .await
    .unwrap();
    fn scan_cost(value: &Value) -> (u64, u64) {
        match value {
            Value::Object(fields) => {
                let mut cost = (
                    fields
                        .get("indexFetches")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    fields
                        .get("docFetches")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                for child in fields.values() {
                    let (indexes, docs) = scan_cost(child);
                    cost.0 += indexes;
                    cost.1 += docs;
                }
                cost
            }
            Value::Array(values) => values
                .iter()
                .map(scan_cost)
                .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)),
            _ => (0, 0),
        }
    }
    let roots = plan["explain"]["operationNode"].as_array().unwrap();
    assert_eq!(roots.len(), 3, "{plan}");
    let request_cost = scan_cost(&roots[0]);
    assert_eq!(
        request_cost,
        (0, 1),
        "request must use its point read: {plan}"
    );
    assert_eq!(
        scan_cost(&roots[1]),
        (0, 0),
        "no headers in this request: {plan}"
    );
    let (indexes, docs) = scan_cost(&roots[2]);
    let request_rows = INERT_ROWS as u64 + 8;
    assert!(
        indexes > 0 && indexes <= request_rows && docs > 0 && docs <= request_rows,
        "live output must touch only the selected request above the estimate cap: {plan}"
    );
    let mut oversized = execute_local_graphql_query(&node, &query, "live cap fixture")
        .await
        .unwrap();
    assert_eq!(
        oversized[AGENT_OUTPUT_SEGMENT_NAME]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        serde_json::to_vec(&oversized).unwrap().len() < 8_192,
        "inert payloads must not cross the query boundary"
    );
    let limited = execute_local_graphql_query(
        &node,
        &query.replace(&format!("limit: {}", MAX_TIP_REQUEST_ROWS + 1), "limit: 4"),
        "live scope before limit",
    )
    .await
    .unwrap();
    assert_eq!(
        limited[AGENT_OUTPUT_SEGMENT_NAME].as_array().unwrap().len(),
        4
    );
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v0/graphql"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({"query": query}),
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": oversized})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let remote = ConfigAccess::graphql(format!("{}/api/v0/graphql", server.uri()));
    assert_eq!(
        load_session_live_store_on(&remote, &request)
            .await
            .unwrap()
            .output_segments
            .len(),
        4
    );
    let record = oversized[AGENT_OUTPUT_SEGMENT_NAME][0].clone();
    oversized[AGENT_OUTPUT_SEGMENT_NAME] = Value::Array(vec![record; MAX_TIP_REQUEST_ROWS + 1]);
    assert!(live_store(&oversized, &request)
        .unwrap_err()
        .to_string()
        .contains("bounded session-tip"));
    let access = ConfigAccess::Local(node.clone());
    let start = std::time::Instant::now();
    let live = load_session_live_store_on(&access, &request).await.unwrap();
    tracing::info!(
        elapsed_ms = start.elapsed().as_secs_f64() * 1000.0,
        segments = live.output_segments.len(),
        "bounded live query regression"
    );
    assert_eq!(live.requests.len(), 1);
    assert_eq!(live.output_segments.len(), 4);
    assert_eq!(
        live.output_segments
            .iter()
            .filter(|s| s.segment.ordinal == Some(0))
            .count(),
        2
    );
    assert_eq!(
        live.output_segments
            .iter()
            .filter(|s| s.segment.ordinal.is_none())
            .count(),
        1
    );
    let local = load_session_live_store(&node, &request).await.unwrap();
    assert_eq!(local.output_segments.len(), live.output_segments.len());
    ConfigAccess::write_local(&node, "test.live_terminal", &format!(r#"mutation {{
        update_AgentRequest(filter: {{_docID: {{_eq:"{doc}"}}}}, input: {{lifecycle_state:"completed"}}) {{ _docID }}
    }}"#)).await.unwrap();
    let completed = load_session_live_store(&node, &request).await.unwrap();
    assert_eq!(
        completed.requests[0].lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
    let wrong_scope = AgentRequestRow {
        requester_did: Some("other".into()),
        ..request.clone()
    };
    assert!(load_session_live_store(&node, &wrong_scope)
        .await
        .unwrap()
        .requests
        .is_empty());
    ConfigAccess::write_local(
        &node,
        "test.live_remove",
        &format!(
            r#"mutation {{
        delete_AgentRequest(filter: {{_docID: {{_eq:"{doc}"}}}}) {{ _docID }}
    }}"#
        ),
    )
    .await
    .unwrap();
    assert!(load_session_live_store(&node, &request)
        .await
        .unwrap()
        .requests
        .is_empty());
}
