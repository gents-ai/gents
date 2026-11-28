//! What changes the session list fingerprint: a row the list shows, never a
//! followed turn's reply pieces.

use super::session_list_fingerprint;
use gents_observe::client_projection::observe::ObservedStores;
use gents_observe::client_projection::store::{ClientStore, ClientStoreRows};
use gents_protocol::output::rows::OutputSegmentRow;
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::{AgentSession, SessionObservation, SessionRequestObservation};

/// One reply piece of the followed turn: an output row.
fn segment(ordinal: u32, text: &str) -> OutputSegmentRow {
    OutputSegmentRow {
        doc_id: format!("segment-{ordinal}"),
        segment: OutputSegment {
            node_did: "node".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "session-request".into(),
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
            stream_bytes_after: None,
            payload: text.into(),
            close: None,
            created_at: "2026-10-07T12:00:00Z".into(),
        },
    }
}

fn session(session_id: &str, lifecycle_state: RequestLifecycleState) -> AgentSession {
    AgentSession {
        session_id: session_id.into(),
        node_did: "node".into(),
        requester_did: None,
        agent_did: "default".into(),
        created_at: "2026-10-07T12:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: Some(SessionObservation {
            last_activity_at: "2026-10-07T12:00:00Z".into(),
            preview: None,
            reply_preview: None,
            latest_request: Some(SessionRequestObservation {
                request_doc_id: format!("{session_id}-request"),
                request_id: format!("{session_id}-logical"),
                lifecycle_state,
            }),
        }),
    }
}

/// The followed session, its turn streaming, beside another session.
fn followed() -> ClientStoreRows {
    ClientStoreRows {
        requests: vec![AgentRequestRow {
            doc_id: Some("session-request".into()),
            request_id: "session-logical".into(),
            session_id: Some("session".into()),
            node_did: Some("node".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            ..Default::default()
        }],
        output_segments: vec![segment(0, "hello")],
        sessions: vec![
            session("session", RequestLifecycleState::Processing),
            session("other", RequestLifecycleState::Completed),
        ],
        ..Default::default()
    }
}

fn fingerprint(rows: &ClientStoreRows) -> u64 {
    let stores = ObservedStores::new();
    stores
        .ensure("node")
        .replace_snapshot(ClientStore::from_rows(rows.clone()));
    session_list_fingerprint(&stores)
}

#[test]
fn a_followed_turns_reply_pieces_leave_the_session_list_fingerprint_equal() {
    let mut rows = followed();
    let before = fingerprint(&rows);
    rows.output_segments.push(segment(1, " world"));
    rows.output_segments.push(segment(2, " again"));
    assert_eq!(fingerprint(&rows), before);
}

#[test]
fn another_sessions_status_changes_the_session_list_fingerprint() {
    let mut rows = followed();
    let before = fingerprint(&rows);
    rows.sessions[1] = session("other", RequestLifecycleState::Processing);
    assert_ne!(fingerprint(&rows), before);

    let mut titled = followed();
    titled.sessions[1]
        .observation
        .as_mut()
        .unwrap()
        .reply_preview = Some("Done".into());
    assert_ne!(fingerprint(&titled), before);
}
