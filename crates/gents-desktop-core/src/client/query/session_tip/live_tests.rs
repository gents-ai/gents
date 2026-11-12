use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::ConfigAccess;

#[tokio::test]
async fn live_read_observes_exact_request_lifecycle_and_every_crdt_record_without_history() {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
    let request = fixture_view_access(&node)
        .read_view(
            &RequestRows::new(Scope::Node("agent".into())).request_ids(vec!["logical".into()]),
        )
        .await
        .unwrap()
        .remove(0);
    let doc = escape_graphql_string(request.doc_id.as_deref().unwrap());
    let source = r#"{kind:"provider_turn", scope:"inference.0", turn_index:0, attempt:0}"#;
    let writer = r#"{kind:"request_execution", execution_generation:"generation"}"#;
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
    let mut oversized = read_tip(&fixture_view_access(&node), &request, true)
        .await
        .unwrap();
    assert_eq!(oversized.segments.len(), 4);
    assert!(
        serde_json::to_vec(&oversized.segments).unwrap().len() < 8192,
        "inert payloads must not cross the query boundary"
    );
    let request_rows = oversized
        .requests
        .iter()
        .map(|row| {
            let mut value = serde_json::to_value(row).unwrap();
            value["_docID"] = serde_json::to_value(&row.doc_id).unwrap();
            value
        })
        .collect::<Vec<_>>();
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v0/graphql"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": {"view_0":request_rows,"view_1":oversized.messages,"view_2":oversized.segments}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let remote = ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::with_authenticated_delegation(
            format!("{}/api/v0/graphql", server.uri()),
            "Bearer fixture",
            node.node_identity_did().unwrap().parse().unwrap(),
        )
        .unwrap(),
    );
    assert_eq!(
        load_session_live_store_on(&remote, &request)
            .await
            .unwrap()
            .output_segments
            .len(),
        4
    );
    let record = oversized.segments[0].clone();
    oversized.segments = vec![record; MAX_TIP_REQUEST_ROWS + 1];
    assert!(live_store(oversized, &request)
        .unwrap_err()
        .to_string()
        .contains("bounded session-tip"));
    let access = fixture_view_access(&node);
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
    let local = load_session_live_store_on(&fixture_view_access(&node), &request)
        .await
        .unwrap();
    assert_eq!(local.output_segments.len(), live.output_segments.len());
    ConfigAccess::write_local(&node, "test.live_terminal", &format!(r#"mutation {{
        update_AgentRequest(filter: {{_docID: {{_eq:"{doc}"}}}}, input: {{lifecycle_state:"completed"}}) {{ _docID }}
    }}"#)).await.unwrap();
    let completed = load_session_live_store_on(&fixture_view_access(&node), &request)
        .await
        .unwrap();
    assert_eq!(
        completed.requests[0].lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
    let wrong_scope = AgentRequestRow {
        requester_did: Some("other".into()),
        ..request.clone()
    };
    assert!(
        load_session_live_store_on(&fixture_view_access(&node), &wrong_scope)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
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
    assert!(
        load_session_live_store_on(&fixture_view_access(&node), &request)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
}

#[tokio::test]
async fn session_live_text_reads_the_running_turn_and_ends_with_its_lifecycle() {
    use gents_protocol::output::rows::OutputSegmentRow;
    use gents_protocol::output::{
        OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let request = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("physical".into()),
        request_id: "logical".into(),
        session_id: Some("session".into()),
        node_did: Some("node".into()),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        execution_generation: Some("generation".into()),
        execution_lease_secs: Some(300),
        execution_lease_expires_at: Some("2026-10-07T12:05:00Z".into()),
        ..Default::default()
    };
    let session: gents_protocol::session::AgentSession =
        serde_json::from_value(serde_json::json!({
            "session_id": "session", "node_did": "node", "agent_did": "agent",
            "created_at": "2026-10-07T12:00:00Z",
        }))
        .unwrap();
    let observed = ClientStore::from_rows(ClientStoreRows {
        requests: vec![request.clone()],
        sessions: vec![session],
        ..Default::default()
    });
    let segment = OutputSegmentRow {
        doc_id: "segment-0".into(),
        segment: OutputSegment {
            node_did: "node".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "physical".into(),
            source: OutputSource::ProviderTurn {
                scope: CaptureScope {
                    kind: CaptureScopeKind::Inference,
                    seq: 0,
                },
                turn_index: 0,
                attempt: 0,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: 5,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: "hello".into(),
            close: None,
            created_at: "2026-10-07T12:00:00Z".into(),
        },
    };
    let mut segment_json = serde_json::to_value(&segment.segment).unwrap();
    segment_json["_docID"] = serde_json::json!(segment.doc_id);
    let mut body = serde_json::json!({"data": {
        "AgentRequest": [request],
        "AgentMessage": [],
        "AgentOutputSegment": [segment_json],
    }});
    body["data"]["AgentRequest"][0]["_docID"] = serde_json::json!("physical");
    let server = wiremock::MockServer::start().await;
    let access = ConfigAccess::graphql(format!("{}/api/v0/graphql", server.uri()));
    let mount = |body: Value| {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/v0/graphql"))
            .and(wiremock::matchers::body_string_contains(
                "DesktopSessionTip",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
    };
    mount(body.clone()).mount(&server).await;
    let live = load_session_live_text_on(&access, &observed, "session", "node")
        .await
        .unwrap()
        .expect("running turn has live text");
    assert_eq!(live.content, "hello");
    assert!(live.reasoning.is_empty());
    server.verify().await;
    server.reset().await;
    body["data"]["AgentRequest"][0]["lifecycle_state"] = serde_json::json!("completed");
    mount(body).mount(&server).await;
    assert!(
        load_session_live_text_on(&access, &observed, "session", "node")
            .await
            .unwrap()
            .is_none()
    );
    server.verify().await;
    assert!(
        load_session_live_text_on(&access, &observed, "session", "other")
            .await
            .unwrap()
            .is_none()
    );
}
