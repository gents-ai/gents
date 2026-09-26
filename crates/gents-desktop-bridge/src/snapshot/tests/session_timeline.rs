use super::*;
use crate::types::{DesktopSessionSnapshot, MessageReconstructionView, ReconstructionState};
use gents_protocol::request_lifecycle::RequestLifecycleState;

fn timeline_session() -> AgentSession {
    AgentSession {
        session_id: "sess-1".into(),
        agent_did: "did:test:amy".into(),
        requester_did: None,
        behavior_id: "default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

fn ready() -> MessageReconstructionView {
    MessageReconstructionView {
        state: ReconstructionState::Ready,
        error: None,
        denied_dependency_doc_id: None,
    }
}

#[test]
fn canonical_headers_and_segments_render_in_sequence() {
    let mut rows = ClientStoreRows {
        sessions: vec![timeline_session()],
        requests: vec![AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some("req-1".into()),
            request_id: "req-1".into(),
            agent_did: Some("did:test:amy".into()),
            session_id: Some("sess-1".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            ..Default::default()
        }],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "user",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "inspect the repository",
    );
    push_canonical_text_message(
        &mut rows,
        "assistant",
        "sess-1",
        Some("req-1"),
        2,
        MessageRole::Assistant,
        "I found the canonical owner.",
    );
    let snapshot = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert_eq!(snapshot.messages.len(), 2);
    assert!(
        matches!(&snapshot.timeline_items[..], [RenderedTimelineItem::UserMessage { content, reconstruction, .. }, RenderedTimelineItem::AssistantMessage { content: assistant, reconstruction: assistant_reconstruction, .. }]
        if content.as_deref() == Some("inspect the repository") && reconstruction.state == ReconstructionState::Ready && assistant.as_deref() == Some("I found the canonical owner.") && assistant_reconstruction.state == ReconstructionState::Ready)
    );
}

#[test]
fn interrupted_queued_steering_keeps_request_owned_input_without_transcript() {
    let rows = ClientStoreRows {
        sessions: vec![timeline_session()],
        requests: vec![AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some("steering-doc-1".into()),
            request_id: "steering-request-1".into(),
            agent_did: Some("did:test:amy".into()),
            session_id: Some("sess-1".into()),
            lifecycle_state: Some(RequestLifecycleState::Interrupted),
            content: Some("queued steering text".into()),
            input: Some(
                serde_json::from_value(serde_json::json!({
                    "queue": { "source": "steering", "policy": "append" }
                }))
                .expect("steering input"),
            ),
            ..Default::default()
        }],
        ..ClientStoreRows::default()
    };
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(rows),
        "sess-1",
        Some("steering-request-1"),
    )
    .expect("snapshot");
    let pending = snapshot.pending_turn.expect("request-owned steering input");
    assert_eq!(pending.request_doc_id.as_deref(), Some("steering-doc-1"));
    assert_eq!(pending.content, "queued steering text");
    assert_eq!(pending.lifecycle_state.as_deref(), Some("interrupted"));
    assert!(snapshot.messages.is_empty());
}

#[test]
fn missing_segment_remains_loading_in_the_timeline() {
    let mut rows = ClientStoreRows {
        sessions: vec![timeline_session()],
        requests: vec![AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some("req-1".into()),
            request_id: "req-1".into(),
            agent_did: Some("did:test:amy".into()),
            session_id: Some("sess-1".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            ..Default::default()
        }],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "header-only",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::Assistant,
        "not a fallback",
    );
    rows.output_segments.clear();
    let snapshot = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert!(
        matches!(snapshot.timeline_items.as_slice(), [RenderedTimelineItem::AssistantMessage { content: None, reconstruction, .. }] if reconstruction.state == ReconstructionState::Loading)
    );
}

#[test]
fn session_timeline_pages_are_bounded_and_cursor_stable() {
    let mut snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![timeline_session()],
            ..ClientStoreRows::default()
        }),
        "sess-1",
        None,
    )
    .expect("snapshot");
    snapshot.timeline_items = (0..100)
        .map(|index| RenderedTimelineItem::UserMessage {
            item_key: format!("message-{index:03}"),
            request_id: Some(format!("request-{index:03}")),
            sequence: Some(index),
            content: Some(format!("row {index}")),
            timestamp: None,
            reconstruction: ready(),
        })
        .collect();
    let mut older_snapshot = snapshot.clone();
    apply_session_timeline_page(&mut snapshot, None, Some(40)).expect("tip page");
    let tip = snapshot.timeline_page.as_ref().expect("metadata");
    assert_eq!(snapshot.timeline_items.len(), 40);
    assert_eq!(tip.oldest_item_key.as_deref(), Some("message-060"));
    assert!(tip.has_older);
    assert!(!tip.has_newer);
    apply_session_timeline_page(&mut older_snapshot, Some("message-060"), Some(40))
        .expect("older page");
    assert_eq!(
        older_snapshot
            .timeline_page
            .as_ref()
            .and_then(|page| page.oldest_item_key.as_deref()),
        Some("message-020")
    );
}

#[test]
fn queried_timeline_page_reports_database_work_and_does_not_rescan_for_cursor() {
    let mut snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![timeline_session()],
            ..ClientStoreRows::default()
        }),
        "sess-1",
        None,
    )
    .expect("snapshot");
    snapshot.timeline_items = (0..81)
        .map(|index| RenderedTimelineItem::UserMessage {
            item_key: format!("message-{index:03}"),
            request_id: None,
            sequence: Some(index),
            content: Some(format!("row {index}")),
            timestamp: None,
            reconstruction: ready(),
        })
        .collect();
    let page = gents_desktop_core::client::SessionTranscriptQueryPage {
        store: ClientStore::default(),
        canonical_dependencies: Default::default(),
        query_count: 2,
        queried_rows: 81,
        message_query_limit: 41,
        tool_call_query_limit: 321,
        source_exhausted: false,
        has_newer: true,
    };
    apply_session_timeline_page_with_query(
        &mut snapshot,
        Some("message-081"),
        Some(40),
        Some(&page),
    )
    .expect("page");
    let metadata = snapshot.timeline_page.expect("metadata");
    assert_eq!(
        (
            metadata.query_count,
            metadata.queried_rows,
            metadata.message_query_limit
        ),
        (Some(2), Some(81), Some(41))
    );
    assert_eq!(metadata.total_items_exact, Some(false));
    assert!(metadata.has_older && metadata.has_newer);
}

#[test]
fn terminal_request_does_not_create_a_mutable_live_tail() {
    let request = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("req-1".into()),
        request_id: "req-1".into(),
        agent_did: Some("did:test:amy".into()),
        session_id: Some("sess-1".into()),
        lifecycle_state: Some(RequestLifecycleState::Completed),
        ..Default::default()
    };
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![timeline_session()],
            requests: vec![request],
            ..ClientStoreRows::default()
        }),
        "sess-1",
        Some("req-1"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

// The response-row overlay tests below retain their UI guarantees under the
// canonical contract: live bytes are no longer a mutable store collection, so
// an active request asks the shared projector for a fresh snapshot instead.
#[test]
fn session_snapshot_consumes_generated_live_overlay_cases() {
    let request = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("req-1".into()),
        request_id: "req-1".into(),
        agent_did: Some("did:test:amy".into()),
        session_id: Some("sess-1".into()),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        ..Default::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![timeline_session()],
        requests: vec![request],
        ..ClientStoreRows::default()
    });
    let delta = build_session_live_delta_from_store(
        &store,
        gents_desktop_core::client::StoreProjectionRevision {
            store_version: 7,
            reconcile_version: 4,
        },
        "sess-1",
        Some("did:test:amy"),
        "req-1",
        4,
        0,
        "811c9dc5",
        0,
        "811c9dc5",
    );
    assert_eq!(delta.outcome, "snapshotRequired");
    assert_eq!(delta.turn_state.as_deref(), Some("running"));
}

#[test]
fn queried_timeline_page_never_splits_a_sequence_group() {
    let mut snapshot = empty_timeline_snapshot();
    snapshot.timeline_items = vec![
        assistant_item("message-1", 1),
        tool_group(1),
        assistant_item("message-2", 2),
        tool_group(2),
        assistant_item("message-3", 3),
    ];
    let page = timeline_page(5, true, false);
    apply_session_timeline_page_with_query(&mut snapshot, None, Some(4), Some(&page))
        .expect("sequence-atomic page");
    assert_eq!(
        timeline_keys(&snapshot),
        ["message-2", "tools-2", "message-3"]
    );
}

#[test]
fn queried_timeline_page_advances_past_non_rendering_rows() {
    let mut snapshot = empty_timeline_snapshot();
    let page = timeline_page(41, false, false);
    apply_session_timeline_page_with_query(&mut snapshot, None, Some(40), Some(&page))
        .expect("empty rendered page");
    assert!(snapshot.timeline_items.is_empty());
    assert!(snapshot.timeline_page.expect("metadata").has_older);
}

#[test]
fn queried_timeline_page_drops_old_orphans_below_the_selected_sequence_window() {
    let mut snapshot = empty_timeline_snapshot();
    snapshot.timeline_items = vec![
        assistant_item("message-2", 2),
        assistant_item("message-3", 3),
        tool_group(3),
        tool_group(1),
    ];
    let page = timeline_page(4, true, false);
    apply_session_timeline_page_with_query(&mut snapshot, None, Some(3), Some(&page))
        .expect("windowed page");
    assert_eq!(
        timeline_keys(&snapshot),
        ["message-2", "message-3", "tools-3"]
    );
}

#[test]
fn live_delta_appends_only_the_new_suffix_and_fences_reconcile_gaps() {
    let store = active_store();
    let revision = gents_desktop_core::client::StoreProjectionRevision {
        store_version: 9,
        reconcile_version: 4,
    };
    let current = build_session_live_delta_from_store(
        &store,
        revision,
        "sess-1",
        Some("did:test:amy"),
        "req-1",
        4,
        5,
        "4f9f2cab",
        0,
        "811c9dc5",
    );
    assert_eq!(current.outcome, "snapshotRequired");
    let fenced = build_session_live_delta_from_store(
        &store,
        revision,
        "sess-1",
        Some("did:test:amy"),
        "req-1",
        3,
        5,
        "4f9f2cab",
        0,
        "811c9dc5",
    );
    assert_eq!(fenced.outcome, "snapshotRequired");
    assert!(fenced.content.is_none());
}

#[test]
fn overlay_hidden_when_response_tail_is_empty() {
    let snapshot = build_session_snapshot_from_store(&active_store(), "sess-1", Some("req-1"))
        .expect("snapshot");
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn background_notification_is_control_by_message_key_with_honest_request_binding() {
    let mut rows = active_store().to_rows();
    push_canonical_text_message(
        &mut rows,
        "background-completion-notification:child-1:subagent",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "wake",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(snapshot.messages[0].runtime_control);
}

#[test]
fn versioned_background_wake_never_projects_as_a_user_turn() {
    let mut rows = active_store().to_rows();
    push_canonical_text_message(
        &mut rows,
        "background-completion-notification:v1:req-1",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "wake",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(snapshot.timeline_items.is_empty());
}

#[test]
fn steering_projects_the_authored_input_once() {
    let mut rows = active_store().to_rows();
    rows.requests[0].input = Some(
        serde_json::from_value(serde_json::json!({
            "queue": { "source": "steering", "policy": "append" }
        }))
        .expect("steering request input"),
    );
    push_canonical_text_message(
        &mut rows,
        "authored:req-1:prompt",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "continue",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(!snapshot.messages[0].runtime_control);
    assert_eq!(
        snapshot
            .timeline_items
            .iter()
            .filter(|item| matches!(item, RenderedTimelineItem::UserMessage { .. }))
            .count(),
        1
    );
}

#[test]
fn durable_goal_continuation_never_projects_as_user_authored_input() {
    let mut rows = active_store().to_rows();
    rows.requests[0].input = Some(
        serde_json::from_value(serde_json::json!({
            "queue": { "source": "goal", "policy": "coalesce" },
            "goal_continuation": { "sequence": 1, "wrapup": false }
        }))
        .expect("goal continuation request input"),
    );
    push_canonical_text_message(
        &mut rows,
        "goal-continuation:req-1",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "continue goal",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(snapshot.messages[0].runtime_control);
}

#[test]
fn session_snapshot_deduplicates_persisted_rows_from_multiple_sources() {
    let mut rows = active_store().to_rows();
    push_canonical_text_message(
        &mut rows,
        "dedupe",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "once",
    );
    let duplicate = rows.transcript_messages[0].clone();
    let duplicate_segment = rows.output_segments[0].clone();
    rows.transcript_messages.push(duplicate);
    rows.output_segments.push(duplicate_segment);
    let first = ClientStore::from_rows(ClientStoreRows {
        transcript_messages: vec![rows.transcript_messages.remove(0)],
        output_segments: vec![rows.output_segments.remove(0)],
        ..ClientStoreRows::default()
    });
    let store = first.merge_snapshot(ClientStore::from_rows(rows));
    assert_eq!(store.transcript("sess-1").messages.len(), 1);
}

#[test]
fn session_snapshot_hides_live_overlay_matching_last_materialized_assistant() {
    let mut rows = active_store().to_rows();
    push_canonical_text_message(
        &mut rows,
        "assistant",
        "sess-1",
        Some("req-1"),
        2,
        MessageRole::Assistant,
        "hello back",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_places_live_overlay_before_running_orphan_tool_group() {
    let snapshot = build_session_snapshot_from_store(&active_store(), "sess-1", Some("req-1"))
        .expect("snapshot");
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_projects_open_canonical_segment_before_header_arrives() {
    let mut rows = active_store().to_rows();
    rows.requests[0].execution_generation = Some("generation-1".into());
    rows.requests[0].execution_lease_secs = Some(300);
    rows.requests[0].execution_lease_expires_at = Some("2026-04-21T12:05:00Z".into());
    push_canonical_text_message(
        &mut rows,
        "authored-prompt",
        "sess-1",
        Some("req-1"),
        1,
        MessageRole::User,
        "start work",
    );
    push_canonical_text_message(
        &mut rows,
        "prior-assistant-tool-turn",
        "sess-1",
        Some("req-1"),
        2,
        MessageRole::Assistant,
        "calling a tool",
    );
    rows.output_segments.push(OutputSegmentRow {
        doc_id: "open-0".into(),
        segment: OutputSegment {
            agent_did: "did:test:amy".into(),
            requester_did: None,
            session_id: "sess-1".into(),
            request_doc_id: "req-1".into(),
            source: OutputSource::ProviderTurn {
                scope: gents_protocol::rendered_request::CaptureScope {
                    kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                    seq: 0,
                },
                turn_index: 1,
                attempt: 0,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation-1".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: 7,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: "working".into(),
            close: None,
            created_at: "2026-04-21T12:00:00Z".into(),
        },
    });
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(snapshot.timeline_items.iter().any(|item| matches!(
        item,
        RenderedTimelineItem::LiveAssistant { content, .. }
            if content.as_deref() == Some("working")
    )));
}

#[test]
fn session_snapshot_hides_failed_unmaterialized_response_overlay() {
    let mut rows = active_store().to_rows();
    rows.requests[0].lifecycle_state = Some(RequestLifecycleState::Failed);
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("failed"));
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_keeps_full_live_overlay_when_only_prior_turn_shares_prefix() {
    let mut rows = active_store().to_rows();
    push_canonical_text_message(
        &mut rows,
        "prior",
        "sess-1",
        Some("prior-request"),
        1,
        MessageRole::Assistant,
        "hello",
    );
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", Some("req-1"))
            .expect("snapshot");
    assert!(snapshot.timeline_items.iter().any(|item| matches!(item, RenderedTimelineItem::AssistantMessage { content, .. } if content.as_deref() == Some("hello"))));
}

#[test]
fn session_snapshot_renders_structured_tool_payloads_in_timeline() {
    let tool = serde_json::from_value(serde_json::json!({
        "_docID": "tool-doc", "agent_did": "did:test:amy", "request_doc_id": "req-1",
        "tool_call_key": "tool-1", "session_id": "sess-1", "request_id": "req-1",
        "message_sequence": 2, "tool_name": "glob", "tool_call_id": "call-1",
        "status": "completed", "lifecycle_state": "completed",
        "completed_at": "2026-04-21T12:00:02Z"
    }))
    .expect("canonical tool call envelope");
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![timeline_session()],
            tool_calls: vec![tool],
            ..ClientStoreRows::default()
        }),
        "sess-1",
        None,
    )
    .expect("snapshot");
    let tools = snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => Some(tools),
            _ => None,
        })
        .expect("tool group");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name, "glob");
    assert_eq!(tools[0].status_kind, "success");
}

#[test]
fn structured_command_policy_denial_projects_to_rendered_tool() {
    let tool = serde_json::from_value(serde_json::json!({
        "_docID": "tool-denial-doc", "agent_did": "did:test:amy", "request_doc_id": "req-denial",
        "tool_call_key": "tool-denial", "session_id": "session-denial", "request_id": "req-denial",
        "message_sequence": 1, "tool_name": "bash", "tool_call_id": "call-denial",
        "status": "completed", "lifecycle_state": "failed", "completed_at": "2026-05-20T10:32:16Z",
        "tool_failure_class": "policyDenied", "denial_reason": "readOnlySubcommandNotAllowlisted",
        "denied_command": "git", "denied_subcommand": "commit", "policy_mode": "read_only",
        "policy_network": "inherit", "latency_ms": 12
    }))
    .expect("canonical denial envelope");
    let session = AgentSession {
        session_id: "session-denial".into(),
        agent_did: "did:test:amy".into(),
        requester_did: None,
        behavior_id: "default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    };
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session],
            tool_calls: vec![tool],
            ..ClientStoreRows::default()
        }),
        "session-denial",
        None,
    )
    .expect("snapshot");
    let tool = snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => tools.first(),
            _ => None,
        })
        .expect("rendered tool");
    let denial = tool.denial.as_ref().expect("structured denial");
    assert_eq!(tool.status_kind, "error");
    assert_eq!(denial.rule_id, "readOnlySubcommandNotAllowlisted");
    assert_eq!(denial.category, "read-only-guard");
    assert_eq!(denial.denied_command.as_deref(), Some("git"));
    assert_eq!(denial.denied_subcommand.as_deref(), Some("commit"));
}

fn empty_timeline_snapshot() -> DesktopSessionSnapshot {
    build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![timeline_session()],
            ..ClientStoreRows::default()
        }),
        "sess-1",
        None,
    )
    .expect("snapshot")
}

fn active_store() -> ClientStore {
    ClientStore::from_rows(ClientStoreRows {
        sessions: vec![timeline_session()],
        requests: vec![AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some("req-1".into()),
            request_id: "req-1".into(),
            agent_did: Some("did:test:amy".into()),
            session_id: Some("sess-1".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            ..Default::default()
        }],
        ..ClientStoreRows::default()
    })
}

fn assistant_item(key: &str, sequence: i64) -> RenderedTimelineItem {
    RenderedTimelineItem::AssistantMessage {
        item_key: key.into(),
        sequence: Some(sequence),
        content: Some(key.into()),
        reasoning: None,
        timestamp: None,
        reconstruction: ready(),
    }
}

fn tool_group(sequence: i64) -> RenderedTimelineItem {
    RenderedTimelineItem::ToolGroup {
        item_key: format!("tools-{sequence}"),
        message_sequence: Some(sequence),
        tools: Vec::new(),
    }
}

fn timeline_page(
    queried_rows: usize,
    source_exhausted: bool,
    has_newer: bool,
) -> gents_desktop_core::client::SessionTranscriptQueryPage {
    gents_desktop_core::client::SessionTranscriptQueryPage {
        store: ClientStore::default(),
        canonical_dependencies: Default::default(),
        query_count: 2,
        queried_rows,
        message_query_limit: queried_rows,
        tool_call_query_limit: 321,
        source_exhausted,
        has_newer,
    }
}

fn timeline_keys(snapshot: &DesktopSessionSnapshot) -> Vec<&str> {
    snapshot
        .timeline_items
        .iter()
        .map(|item| match item {
            RenderedTimelineItem::UserMessage { item_key, .. }
            | RenderedTimelineItem::AssistantMessage { item_key, .. }
            | RenderedTimelineItem::ToolGroup { item_key, .. }
            | RenderedTimelineItem::PendingUserTurn { item_key, .. }
            | RenderedTimelineItem::LiveAssistant { item_key, .. } => item_key.as_str(),
        })
        .collect()
}
