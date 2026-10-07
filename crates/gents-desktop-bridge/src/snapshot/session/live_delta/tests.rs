use super::*;
use gents::session::canonical_rows::OutputSegmentRow;
use gents_desktop_core::client::{ClientStoreRows, StoreProjectionRevision};
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

fn rows() -> ClientStoreRows {
    ClientStoreRows {
        requests: vec![AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some("physical".into()),
            request_id: "logical".into(),
            session_id: Some("session".into()),
            agent_did: Some("agent".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            execution_generation: Some("generation".into()),
            execution_lease_secs: Some(300),
            execution_lease_expires_at: Some("2026-10-07T12:05:00Z".into()),
            ..Default::default()
        }],
        output_segments: vec![segment(0, "hello")],
        ..Default::default()
    }
}

fn segment(ordinal: u32, text: &str) -> OutputSegmentRow {
    OutputSegmentRow {
        doc_id: format!("segment-{ordinal}"),
        segment: OutputSegment {
            agent_did: "agent".into(),
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
            ordinal: Some(ordinal),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: text.len() as u32,
                declaration: (ordinal == 0).then_some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.into(),
            close: None,
            created_at: "2026-10-07T12:00:00Z".into(),
        },
    }
}

fn cursor(store: &ClientStore) -> String {
    canonical_live_text(store, store, "session", Some("agent"), "logical")
        .unwrap()
        .cursor
}

fn delta(store: &ClientStore, cursor: &str) -> SessionLiveDeltaView {
    build_session_live_delta_from_store(
        store,
        StoreProjectionRevision { store_version: 900 },
        "session",
        Some("agent"),
        "logical",
        cursor,
        5,
        "4f9f2cab",
        0,
        "811c9dc5",
    )
}

#[test]
fn generated_live_cursor_contract() {
    let proofs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs");
    let output = std::process::Command::new("lake")
        .args([
            "env",
            "lean",
            "--run",
            "Proofs/Conformance/ClientObservationOrdering.lean",
        ])
        .current_dir(proofs)
        .output()
        .expect("execute Lean live cursor owner");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let cases = data["liveDeltas"].as_array().unwrap();
    assert_eq!(cases.len(), 18);
    for case in cases {
        let base = case["base"].as_u64().map(|v| v.to_string());
        let current = case["current"].as_u64().map(|v| v.to_string());
        assert_eq!(
            accepts_live_cursor(
                base.as_deref(),
                current.as_deref(),
                case["terminal"].as_bool().unwrap()
            ),
            case["accepted"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn observed_store_can_stay_payload_free_while_scoped_rows_produce_a_verified_suffix() {
    let mut rows = rows();
    let initial = ClientStore::from_rows(rows.clone());
    let cursor = cursor(&initial);
    let (observed, _) = gents_desktop_core::client::ObservedStore::new(initial);
    assert!(observed.snapshot().output_segments.is_empty());
    rows.output_segments.push(segment(1, " world 🌍"));
    let store = ClientStore::from_rows(rows);
    let result = delta(&store, &cursor);
    assert_eq!(result.outcome, "delta");
    assert_eq!(result.live_cursor.as_deref(), Some(cursor.as_str()));
    let patch = result.content.unwrap();
    assert_eq!(patch.mode, "append");
    assert_eq!(patch.value, " world 🌍");
    assert_eq!(patch.byte_len, "hello world 🌍".len());
    assert_eq!(patch.hash, live_text_hash("hello world 🌍"));
}

#[test]
fn a_new_attempt_cannot_patch_the_previous_source_even_with_identical_text() {
    let mut rows = rows();
    let old = cursor(&ClientStore::from_rows(rows.clone()));
    if let OutputSource::ProviderTurn { attempt, .. } = &mut rows.output_segments[0].segment.source
    {
        *attempt += 1;
    }
    let store = ClientStore::from_rows(rows);
    assert_ne!(cursor(&store), old);
    assert_eq!(delta(&store, &old).outcome, "snapshotRequired");
}

#[test]
fn fresh_terminal_or_missing_request_requires_a_snapshot() {
    let mut rows = rows();
    let old = cursor(&ClientStore::from_rows(rows.clone()));
    rows.requests[0].lifecycle_state = Some(RequestLifecycleState::Completed);
    assert_eq!(
        delta(&ClientStore::from_rows(rows.clone()), &old).outcome,
        "snapshotRequired"
    );
    rows.requests.clear();
    assert_eq!(
        delta(&ClientStore::from_rows(rows), &old).outcome,
        "snapshotRequired"
    );
}

#[test]
fn snapshot_and_delta_preserve_the_same_markdown_bytes() {
    let mut rows = rows();
    rows.sessions.push(gents_protocol::session::AgentSession {
        session_id: "session".into(),
        agent_did: "agent".into(),
        requester_did: None,
        behavior_id: "default".into(),
        created_at: "2026-10-07T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    });
    rows.output_segments = vec![segment(0, "  hello\r\n\r\n\r\nworld  ")];
    let store = ClientStore::from_rows(rows);
    let snapshot =
        crate::snapshot::build_session_snapshot_from_store(&store, "session", Some("logical"))
            .unwrap();
    let content = snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            crate::types::RenderedTimelineItem::LiveAssistant { content, .. } => content.as_deref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(content, "hello\r\n\r\n\r\nworld");
    let result = build_session_live_delta_from_store(
        &store,
        StoreProjectionRevision { store_version: 1 },
        "session",
        Some("agent"),
        "logical",
        snapshot.live_cursor.as_deref().unwrap(),
        content.len(),
        &live_text_hash(content),
        0,
        "811c9dc5",
    );
    assert_eq!(result.outcome, "unchanged");
}

#[tokio::test]
async fn operator_delta_uses_fresh_rows_with_a_payload_free_observer_and_snapshot_cursor() {
    use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};
    let home = tempfile::tempdir().unwrap();
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(home.path()),
        ClientCoreOptions::local_simulated_route(),
    )
    .await
    .unwrap();
    let server = wiremock::MockServer::start().await;
    core.add_local_standard_peer_route_for_test(
        "Live test",
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
        "agent",
        &format!("{}/api/v0/graphql", server.uri()),
        home.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    let mut rows = rows();
    rows.requests[0].requester_did = Some("agent".into());
    rows.output_segments[0].segment.requester_did = Some("agent".into());
    rows.sessions.push(gents_protocol::session::AgentSession {
        session_id: "session".into(),
        agent_did: "agent".into(),
        requester_did: Some("agent".into()),
        behavior_id: "default".into(),
        created_at: "2026-10-07T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    });
    let full = ClientStore::from_rows(rows.clone());
    core.store().merge_observer_patch(full.clone());
    let observed = core.store().snapshot();
    assert!(observed.output_segments.is_empty());
    let cursor = canonical_live_text(&observed, &full, "session", Some("agent"), "logical")
        .unwrap()
        .cursor;
    rows.requests[0].execution_lease_expires_at = Some("2026-10-07T12:10:00Z".into());
    let mut append = segment(1, " world");
    append.segment.requester_did = Some("agent".into());
    rows.output_segments.push(append);
    let segments: Vec<_> = rows
        .output_segments
        .iter()
        .map(|row| {
            let mut value = serde_json::to_value(&row.segment).unwrap();
            value["_docID"] = serde_json::json!(row.doc_id);
            value
        })
        .collect();
    let mut body = serde_json::json!({"data": {
        "AgentRequest": rows.requests,
        "AgentMessage": [],
        "AgentOutputSegment": segments,
    }});
    body["data"]["AgentRequest"][0]["_docID"] = serde_json::json!(full.requests[0].doc_id);
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v0/graphql"))
        .and(wiremock::matchers::body_string_contains(
            "DesktopSessionTip",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&body))
        .expect(1)
        .mount(&server)
        .await;
    let result = build_session_live_delta(
        &core,
        "session",
        Some("agent"),
        "logical",
        &cursor,
        5,
        &live_text_hash("hello"),
        0,
        &live_text_hash(""),
    )
    .await
    .unwrap();
    assert_eq!(result.outcome, "delta");
    assert_eq!(result.content.unwrap().value, " world");
    assert_eq!(result.live_cursor.as_deref(), Some(cursor.as_str()));
    let missing = build_session_live_delta(
        &core,
        "session",
        Some("agent"),
        "missing",
        &cursor,
        5,
        &live_text_hash("hello"),
        0,
        &live_text_hash(""),
    )
    .await
    .unwrap();
    assert_eq!(missing.outcome, "snapshotRequired");
    server.verify().await;
    server.reset().await;
    body["data"]["AgentRequest"][0]["lifecycle_state"] = serde_json::json!("completed");
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v0/graphql"))
        .and(wiremock::matchers::body_string_contains(
            "DesktopSessionTip",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let terminal = build_session_live_delta(
        &core,
        "session",
        Some("agent"),
        "logical",
        &cursor,
        5,
        &live_text_hash("hello"),
        0,
        &live_text_hash(""),
    )
    .await
    .unwrap();
    assert_eq!(terminal.outcome, "snapshotRequired");
    assert!(core.store().snapshot().output_segments.is_empty());
    core.shutdown().await.unwrap();
}
