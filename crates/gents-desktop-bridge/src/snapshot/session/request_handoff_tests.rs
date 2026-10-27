use super::*;
use crate::snapshot::tests::push_canonical_text_message;
use crate::types::RenderedTimelineItem;
use gents_desktop_core::client::{ClientStoreRows, RequestPromptFact, RequestPromptOwnership};
use gents_protocol::message::{DocumentSourceKind, Message, UserContent};
use gents_protocol::output::{
    MediaBlock, MediaData, MediaKind, MediaType, MessageBlock, MessageRole, PayloadRef, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload,
};
use gents_protocol::request_input::{QueueDelivery, RequestInput};

fn requests() -> Vec<AgentRequestRow> {
    let mut requests = vec![AgentRequestRow {
        doc_id: Some("doc-head".into()),
        request_id: "head".into(),
        node_did: Some("did:test:amy".into()),
        session_id: Some("session-1".into()),
        content: Some("head text".into()),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        created_at: Some("2026-10-26T08:00:00Z".into()),
        ..Default::default()
    }];
    for index in 1..=3 {
        requests.push(AgentRequestRow {
            doc_id: Some(format!("doc-steer-{index}")),
            request_id: format!("steer-{index}"),
            content: Some(format!("exact steer {index}")),
            lifecycle_state: Some(RequestLifecycleState::Pending),
            created_at: Some(format!("2026-10-26T08:00:0{index}Z")),
            input: Some(serde_json::from_value::<RequestInput>(serde_json::json!({
                "captured_context": [{
                    "kind": "file", "source": "editor", "capturedAt": "2026-10-26T08:00:00Z",
                    "uri": format!("file:///workspace/context-{index}.rs"),
                    "unsaved": true, "content": format!("frozen context {index}")
                }],
                "attachments": [{
                    "name": format!("attachment-{index}.txt"), "mediaType": "text/plain", "data": "ZmlsZQ=="
                }],
                "queue": {
                    "source": "user", "policy": "append", "delivery": "steer", "queued_after_request_id": "head"
                }
            })).unwrap()),
            ..requests[0].clone()
        });
    }
    requests
}

fn store() -> ClientStore {
    ClientStore::from_rows(ClientStoreRows {
        sessions: vec![serde_json::from_value(serde_json::json!({
            "session_id": "session-1", "node_did": "did:test:amy", "agent_id": "agent",
            "created_at": "2026-10-26T08:00:00Z"
        }))
        .unwrap()],
        ..Default::default()
    })
}

fn ownership(requests: &[AgentRequestRow]) -> RequestPromptOwnership {
    RequestPromptOwnership {
        by_request_doc_id: requests
            .iter()
            .map(|request| {
                (
                    request.doc_id.clone().unwrap(),
                    RequestPromptFact {
                        node_did: request.node_did.clone().unwrap(),
                        session_id: request.session_id.clone().unwrap(),
                        requester_did: request.requester_did.clone(),
                        materialized: false,
                        first_sequence: None,
                    },
                )
            })
            .collect(),
    }
}

fn project(
    page: &ClientStore,
    context: &ClientStore,
    facts: &RequestPromptOwnership,
    requests: &[AgentRequestRow],
    live: bool,
) -> DesktopSessionSnapshot {
    build_session_snapshot_from_store_for_node_with_transcript(
        &store(),
        page,
        context,
        None,
        Some(facts),
        true,
        false,
        false,
        live,
        Some("did:test:amy"),
        "session-1",
        Some("head"),
        Some(requests),
    )
    .unwrap()
}

fn inputs(snapshot: &DesktopSessionSnapshot) -> Vec<(&str, &str)> {
    snapshot
        .timeline_items
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::PendingUserTurn {
                request_id,
                content,
                ..
            } => Some((request_id.as_str(), content.as_str())),
            RenderedTimelineItem::UserMessage {
                input_request_id: Some(id),
                content: Some(content),
                ..
            } => Some((id.as_str(), content.as_str())),
            _ => None,
        })
        .collect()
}

fn assert_metadata(snapshot: &DesktopSessionSnapshot, requests: &[AgentRequestRow]) {
    for request in requests.iter().skip(1).take(3) {
        let source = request.input.as_ref().unwrap();
        source.validate_attachments().unwrap();
        let metadata = snapshot
            .timeline_items
            .iter()
            .find_map(|item| match item {
                RenderedTimelineItem::PendingUserTurn {
                    request_id,
                    captured_context,
                    attachments,
                    ..
                } if request_id == &request.request_id => Some((captured_context, attachments)),
                RenderedTimelineItem::UserMessage {
                    input_request_id: Some(id),
                    captured_context,
                    attachments,
                    ..
                } if id == &request.request_id => Some((captured_context, attachments)),
                _ => None,
            })
            .unwrap();
        assert_eq!(metadata.0.as_ref(), Some(&source.captured_context));
        assert_eq!(
            metadata.1.as_ref(),
            Some(
                &source
                    .attachments
                    .iter()
                    .map(|a| a.view())
                    .collect::<Vec<_>>()
            )
        );
    }
}

fn materialize_metadata(rows: &mut ClientStoreRows, request: &AgentRequestRow) {
    let input = request.input.clone().unwrap_or_default();
    let Message::User { content } = input
        .authored_message(request.content.as_deref().unwrap())
        .unwrap()
    else {
        unreachable!()
    };
    let header = &mut rows.transcript_messages.last_mut().unwrap().message;
    let segment_row = rows.output_segments.last_mut().unwrap();
    let segment = &mut segment_row.segment;
    for part in content {
        let UserContent::Document(document) = part else {
            continue;
        };
        let SourceClose::Closed { stream_bytes, .. } = segment.close.as_mut().unwrap() else {
            unreachable!()
        };
        let stream = u32::try_from(stream_bytes.len()).unwrap();
        let reference = PayloadRef {
            close_doc_id: segment_row.doc_id.clone(),
            stream,
        };
        let (data, bytes) = match document.data {
            DocumentSourceKind::String(bytes) => (MediaData::String { data: reference }, bytes),
            DocumentSourceKind::Base64(bytes) => (MediaData::Base64 { data: reference }, bytes),
            _ => panic!("fixture documents carry frozen payloads"),
        };
        segment.runs.push(SegmentRun {
            stream,
            bytes: u32::try_from(bytes.len()).unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: u32::try_from(header.blocks.len()).unwrap(),
                part_index: 0,
                payload: StreamPayload::Media {
                    media_kind: MediaKind::Document,
                },
            }),
        });
        stream_bytes.push(u64::try_from(bytes.len()).unwrap());
        segment.payload.push_str(&bytes);
        header.blocks.push(MessageBlock::Media(MediaBlock {
            kind: MediaKind::Document,
            data,
            media_type: document.media_type.map(MediaType::Document),
            detail: None,
            additional_params: document.additional_params,
        }));
    }
}

#[test]
fn fresh_multi_steer_handoff_retains_each_capture_and_attachment_in_requester_scope() {
    let mut requests = requests();
    let mut queued = requests[1].clone();
    queued.doc_id = Some("doc-queued".into());
    queued.request_id = "queued".into();
    queued
        .input
        .as_mut()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .delivery = QueueDelivery::Queue;
    requests.push(queued);
    for (id, requester, node, session) in [
        (
            "foreign-requester",
            Some("did:test:other"),
            "did:test:amy",
            "session-1",
        ),
        ("foreign-node", None, "did:test:other", "session-1"),
        ("foreign-session", None, "did:test:amy", "session-2"),
    ] {
        requests.push(AgentRequestRow {
            request_id: id.into(),
            doc_id: Some(format!("doc-{id}")),
            requester_did: requester.map(str::to_owned),
            node_did: Some(node.into()),
            session_id: Some(session.into()),
            ..requests[1].clone()
        });
    }
    let facts = ownership(&requests);
    let empty = ClientStore::default();
    for folded in [false, true] {
        if folded {
            for request in &mut requests[1..4] {
                request.lifecycle_state = Some(RequestLifecycleState::Superseded);
                request.failure_reason = Some(gents::lifecycle::FOLDED_REASON.into());
                request.superseded_by_request = Some("head".into());
                request.superseded_by_request_doc_id = Some("doc-head".into());
            }
        }
        let snapshot = project(&empty, &empty, &facts, &requests, true);
        assert_eq!(
            inputs(&snapshot),
            [
                ("head", "head text"),
                ("steer-1", "exact steer 1"),
                ("steer-2", "exact steer 2"),
                ("steer-3", "exact steer 3"),
            ]
        );
        assert_metadata(&snapshot, &requests);
        assert!(snapshot
            .queued_turns
            .iter()
            .any(|turn| turn.request_id == "queued"));
        assert!(snapshot
            .queued_turns
            .iter()
            .all(|turn| !turn.request_id.starts_with("foreign")));
    }
    assert!(inputs(&project(&empty, &empty, &facts, &requests, false)).is_empty());
}

#[test]
fn multiple_folded_inputs_materialize_once_and_stay_on_their_original_page() {
    let mut requests = requests();
    for request in &mut requests[1..] {
        request.lifecycle_state = Some(RequestLifecycleState::Superseded);
        request.failure_reason = Some(gents::lifecycle::FOLDED_REASON.into());
        request.superseded_by_request = Some("head".into());
        request.superseded_by_request_doc_id = Some("doc-head".into());
    }
    let mut facts = ownership(&requests);
    let mut rows = ClientStoreRows::default();
    for (index, request) in requests.iter().enumerate() {
        let sequence = u32::try_from(index + 1).unwrap();
        let key = if index == 0 {
            "authored:doc-head:prompt".into()
        } else {
            format!(
                "authored:doc-head:folded:{}",
                request.doc_id.as_deref().unwrap()
            )
        };
        push_canonical_text_message(
            &mut rows,
            &key,
            "session-1",
            Some("doc-head"),
            sequence,
            MessageRole::User,
            request.content.as_deref().unwrap(),
        );
        materialize_metadata(&mut rows, request);
        let fact = facts
            .by_request_doc_id
            .get_mut(request.doc_id.as_deref().unwrap())
            .unwrap();
        fact.materialized = true;
        fact.first_sequence = Some(i64::from(sequence));
        let context = ClientStore::from_rows(rows.clone());
        let snapshot = project(&context, &context, &facts, &requests, true);
        assert_eq!(
            inputs(&snapshot),
            [
                ("head", "head text"),
                ("steer-1", "exact steer 1"),
                ("steer-2", "exact steer 2"),
                ("steer-3", "exact steer 3"),
            ]
        );
        assert_metadata(&snapshot, &requests);
    }
    let context = ClientStore::from_rows(rows.clone());
    let mut historical = rows.clone();
    historical
        .transcript_messages
        .retain(|row| row.message.sequence < 4);
    let history = project(
        &ClientStore::from_rows(historical),
        &context,
        &facts,
        &requests,
        false,
    );
    assert_eq!(
        inputs(&history),
        [
            ("head", "head text"),
            ("steer-1", "exact steer 1"),
            ("steer-2", "exact steer 2"),
        ]
    );
    rows.transcript_messages
        .retain(|row| row.message.sequence == 4);
    let tip = project(
        &ClientStore::from_rows(rows),
        &context,
        &facts,
        &requests,
        true,
    );
    assert_eq!(inputs(&tip), [("steer-3", "exact steer 3")]);
    assert!(!tip
        .timeline_items
        .iter()
        .any(|row| matches!(row, RenderedTimelineItem::PendingUserTurn { .. })));
}
