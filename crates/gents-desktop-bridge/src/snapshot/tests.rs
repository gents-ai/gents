use gents::agent::p2p_reconcile::session_hydration::{
    ClientHydrationPhase, ClientHydrationProgress,
};
use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_desktop_core::client::{ClientStore, ClientStoreRows};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::row::{AgentRequestRow, GoalRow};
use gents_protocol::session::{
    AgentSession, SessionObservation, SessionRequestObservation, SessionTitle, SessionTitleSource,
};

use super::super::types::InitConfigStatus;
use super::super::types::RenderedTimelineItem;
use super::apply_session_timeline_page;
use super::apply_session_timeline_page_with_query;
use super::build_session_live_delta_from_store;
use super::build_session_snapshot_from_store;
use super::build_session_snapshot_from_store_for_node;
use super::read_stored_init_config;
use super::recent_runs_for_task_views;
use super::scoped_hydration_observation_view;
use super::session_summaries;
use super::task_run_histories;

#[test]
fn supplied_hydration_observation_is_projected_only_for_its_exact_session_and_node() {
    let observation = ClientHydrationProgress {
        session_id: "session-a".into(),
        node_did: "did:test:node-a".into(),
        phase: ClientHydrationPhase::Requested,
        ..Default::default()
    };

    let view = scoped_hydration_observation_view(
        Some((observation.clone(), None)),
        "session-a",
        "did:test:node-a",
    )
    .expect("matching observation");
    assert_eq!(view.session_id, "session-a");
    assert_eq!(view.node_did, "did:test:node-a");
    assert_eq!(view.phase, "requested");

    assert!(scoped_hydration_observation_view(
        Some((observation.clone(), None)),
        "session-b",
        "did:test:node-a",
    )
    .is_none());
    assert!(scoped_hydration_observation_view(
        Some((observation, None)),
        "session-a",
        "did:test:node-b",
    )
    .is_none());
}

#[test]
fn bootstrap_init_config_status_distinguishes_absence_from_unreadable_and_configured() {
    let home = tempfile::tempdir().expect("node home");

    let (config, status) = read_stored_init_config(home.path());
    assert!(config.is_none());
    assert_eq!(status, InitConfigStatus::Absent);

    std::fs::write(home.path().join("init.json"), b"{").expect("write malformed init config");
    let (config, status) = read_stored_init_config(home.path());
    assert!(config.is_none());
    assert_eq!(status, InitConfigStatus::Unreadable);

    std::fs::write(
        home.path().join("init.json"),
        br#"{"home":"/node","node_name":"Engineer","node_did":"did:key:home","tool_ceiling":"meta_only"}"#,
    )
    .expect("write valid init config");
    let (config, status) = read_stored_init_config(home.path());
    assert_eq!(status, InitConfigStatus::Configured);
    assert_eq!(
        config.expect("decoded init config").node_did,
        "did:key:home"
    );
}

fn canonical_text_message(
    doc_id: &str,
    session_id: &str,
    request_doc_id: Option<&str>,
    sequence: u32,
    role: MessageRole,
    text: &str,
) -> (TranscriptMessageRow, OutputSegmentRow) {
    let close_doc_id = format!("{doc_id}:close");
    let source = OutputSource::Authored {
        key: format!("{doc_id}:text"),
    };
    let segment = OutputSegmentRow {
        doc_id: close_doc_id.clone(),
        segment: OutputSegment {
            node_did: "did:test:amy".to_string(),
            requester_did: None,
            session_id: session_id.to_string(),
            request_doc_id: request_doc_id.unwrap_or("fork-origin-request").to_string(),
            source,
            writer: OutputWriter::RequestExecution {
                execution_generation: "test-generation".to_string(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: u32::try_from(text.len()).expect("test text length"),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.to_string(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![u64::try_from(text.len()).expect("test text length")],
            }),
            created_at: "2026-04-21T12:00:00Z".to_string(),
        },
    };
    let header = TranscriptMessageRow {
        doc_id: doc_id.to_string(),
        message: TranscriptMessage {
            message_key: doc_id.to_string(),
            session_id: session_id.to_string(),
            node_did: "did:test:amy".to_string(),
            requester_did: None,
            request_doc_id: request_doc_id.map(str::to_string),
            publication: MessagePublication::RequestExecution {
                execution_generation: "test-generation".to_string(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role,
            native_id: None,
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
            created_at: "2026-04-21T12:00:00Z".to_string(),
        },
    };
    (header, segment)
}

pub(super) fn push_canonical_text_message(
    rows: &mut ClientStoreRows,
    doc_id: &str,
    session_id: &str,
    request_doc_id: Option<&str>,
    sequence: u32,
    role: MessageRole,
    text: &str,
) {
    let (header, segment) =
        canonical_text_message(doc_id, session_id, request_doc_id, sequence, role, text);
    rows.transcript_messages.push(header);
    rows.output_segments.push(segment);
}

#[expect(
    clippy::too_many_arguments,
    reason = "session_basic.rs and session_state.rs call this positionally"
)]
fn push_canonical_text_message_for_node(
    rows: &mut ClientStoreRows,
    doc_id: &str,
    session_id: &str,
    request_doc_id: Option<&str>,
    sequence: u32,
    role: MessageRole,
    text: &str,
    node_did: &str,
    requester_did: Option<&str>,
) {
    let (mut header, mut segment) =
        canonical_text_message(doc_id, session_id, request_doc_id, sequence, role, text);
    header.message.node_did = node_did.to_string();
    header.message.requester_did = requester_did.map(str::to_string);
    segment.segment.node_did = node_did.to_string();
    segment.segment.requester_did = requester_did.map(str::to_string);
    rows.transcript_messages.push(header);
    rows.output_segments.push(segment);
}

#[path = "../../../gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;
#[path = "tests/mcp_health.rs"]
mod mcp_health;
#[path = "tests/runtime.rs"]
mod runtime;
#[path = "tests/session_automated.rs"]
mod session_automated;
#[path = "tests/session_basic.rs"]
mod session_basic;
#[path = "tests/session_queue.rs"]
mod session_queue;
#[path = "tests/session_stale_rows.rs"]
mod session_stale_rows;
#[path = "tests/session_state.rs"]
mod session_state;
#[path = "tests/session_timeline.rs"]
mod session_timeline;
#[path = "tests/session_titles.rs"]
mod session_titles;
#[path = "tests/sync.rs"]
mod sync;
