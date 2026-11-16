use super::*;
use gents_protocol::output::MessagePublication;

/// The transcript a running runtime published in `session_id` for a Goal
/// opening (`source_prompt`, which started `goal_id`) followed by ordinary
/// turns whose first prompt is `followup_prompt` pages and forks with exactly
/// one Goal marker on the opening and its copies.
pub(crate) async fn assert_paged_native_forks(
    access: &gents_store::config_client::ConfigAccess,
    session: &gents_protocol::session::AgentSession,
    goal_id: &str,
    source_prompt: &str,
    followup_prompt: &str,
) {
    use gents_session::session::{ForkBoundary, ForkParams};

    let source = session.session_id.as_str();
    assert_native_pages(access, session, source, goal_id, 10, Some(source_prompt)).await;
    let first = gents_session::session::fork_on(
        access,
        ForkParams {
            source_session_id: source,
            boundary: ForkBoundary::UserTurn(3),
            caller_node_did: &session.node_did,
            caller_requester_did: session.requester_did.as_deref(),
            target_agent_did: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.copied_messages, 6);
    assert_native_pages(
        access,
        session,
        &first.session_id,
        goal_id,
        6,
        Some(source_prompt),
    )
    .await;
    let second = gents_session::session::fork_on(
        access,
        ForkParams {
            source_session_id: &first.session_id,
            boundary: ForkBoundary::UserTurn(2),
            caller_node_did: &session.node_did,
            caller_requester_did: session.requester_did.as_deref(),
            target_agent_did: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.copied_messages, 4);
    assert_native_pages(
        access,
        session,
        &second.session_id,
        goal_id,
        4,
        Some(source_prompt),
    )
    .await;
    for (key, child, count, marked) in [
        (followup_prompt, "edit-followup", 2, true),
        (source_prompt, "edit-opening", 0, false),
    ] {
        let copied = gents_session::session::fork_edit_on(
            access,
            ForkParams {
                source_session_id: source,
                boundary: ForkBoundary::BeforeMessageKey(key),
                caller_node_did: &session.node_did,
                caller_requester_did: session.requester_did.as_deref(),
                target_agent_did: None,
            },
            child,
            &session.node_did,
        )
        .await
        .unwrap();
        assert_eq!(copied, child);
        assert_native_pages(
            access,
            session,
            child,
            goal_id,
            count,
            marked.then_some(source_prompt),
        )
        .await;
    }
}

async fn assert_native_pages(
    access: &gents_store::config_client::ConfigAccess,
    source_session: &gents_protocol::session::AgentSession,
    session_id: &str,
    goal_id: &str,
    expected_items: usize,
    opening_key: Option<&str>,
) {
    use gents_desktop_core::client::{ClientStore, TranscriptWindow};
    let mut cursor = None;
    let mut keys = BTreeSet::new();
    let mut markers = Vec::new();
    for _ in 0..20 {
        let page = gents_desktop_core::client::load_session_transcript_window_on(
            access,
            session_id,
            Some(&source_session.node_did),
            source_session.requester_did.as_deref(),
            cursor.map_or(TranscriptWindow::Tip, TranscriptWindow::Before),
            Some(2),
        )
        .await
        .unwrap();
        if let Some(message) = page
            .store
            .transcript_messages
            .iter()
            .find(|row| Some(row.message.message_key.as_str()) == opening_key)
        {
            let request = page
                .requests
                .iter()
                .find(|row| row.doc_id == message.message.request_doc_id)
                .expect("the opening page retains its exact physical request");
            assert_eq!(
                gents_automation::goal::load_applied_opening_goal_id(access, request)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(goal_id)
            );
        }
        let rows = super::super::render_transcript_rows_with_provenance(
            access,
            &ClientStore::default(),
            &page,
            &source_session.node_did,
            session_id,
        )
        .await
        .expect("transcript provenance read");
        assert!(
            rows.len() <= 2,
            "{session_id} must retain the requested page bound"
        );
        for item in &rows {
            match item {
                RenderedTimelineItem::UserMessage {
                    item_key,
                    started_goal_id,
                    ..
                } => {
                    assert!(
                        keys.insert(item_key.clone()),
                        "{session_id} must not repeat a page"
                    );
                    if let Some(goal) = started_goal_id {
                        markers.push((item_key.clone(), goal.clone()));
                    }
                }
                RenderedTimelineItem::AssistantMessage { item_key, .. } => {
                    assert!(
                        keys.insert(item_key.clone()),
                        "{session_id} must not repeat a page"
                    );
                }
                other => panic!("{session_id} published an unexpected item: {other:?}"),
            }
        }
        if page.source_exhausted {
            break;
        }
        let next = page
            .covered
            .from
            .expect("an older page requires a sequence");
        assert_ne!(cursor, Some(next));
        cursor = Some(next);
    }
    assert_eq!(
        keys.len(),
        expected_items,
        "{session_id} must preserve every persisted message"
    );
    assert_eq!(
        markers.len(),
        usize::from(opening_key.is_some()),
        "{session_id} opening count"
    );
    if let Some((key, goal)) = markers.first() {
        assert_eq!(goal, goal_id);
        if session_id == source_session.session_id {
            assert_eq!(Some(key.as_str()), opening_key);
        }
    }
}

pub(crate) async fn assert_copied_opening(
    access: &gents_store::config_client::ConfigAccess,
    rows: &gents_desktop_core::client::ClientStoreRows,
) {
    use gents_desktop_core::client::{ClientStore, ClientStoreRows};
    let mut child = rows.sessions[0].clone();
    child.session_id = "child".into();
    let mut headers = rows.transcript_messages.clone();
    for original in &rows.transcript_messages {
        let mut first = original.clone();
        first.doc_id = format!("copy:{}", original.doc_id);
        first.message.session_id = "parent-copy".into();
        first.message.message_key = format!("parent:{}", original.message.sequence);
        first.message.request_doc_id = None;
        first.message.publication = MessagePublication::Fork {
            origin_message_doc_id: original.doc_id.clone(),
        };
        let mut second = first.clone();
        second.doc_id = format!("second:{}", original.doc_id);
        second.message.session_id = "child".into();
        second.message.message_key = format!("child:{}", original.message.sequence);
        second.message.publication = MessagePublication::Fork {
            origin_message_doc_id: first.doc_id.clone(),
        };
        headers.extend([first, second]);
    }
    let page = headers
        .iter()
        .filter(|row| row.message.session_id == "child")
        .cloned()
        .collect::<Vec<_>>();
    let original_prompt = rows
        .transcript_messages
        .iter()
        .find(|row| row.message.message_key.ends_with(":prompt"))
        .unwrap();
    for fault in ["none", "denied", "missing", "tampered", "foreign"] {
        let mut dependencies = CanonicalTranscriptDependencies {
            origin_headers: headers
                .iter()
                .filter(|row| row.message.session_id != "child")
                .cloned()
                .collect(),
            origin_requests: rows.requests.clone(),
            output_segments: rows.output_segments.clone(),
            ..Default::default()
        };
        match fault {
            "denied" => dependencies
                .denied_header_doc_ids
                .push(original_prompt.doc_id.clone()),
            "missing" => dependencies
                .origin_headers
                .retain(|row| row.doc_id != original_prompt.doc_id),
            "tampered" => {
                dependencies
                    .origin_headers
                    .iter_mut()
                    .find(|row| row.doc_id == original_prompt.doc_id)
                    .unwrap()
                    .message
                    .created_at = "other".into()
            }
            "foreign" => {
                dependencies
                    .origin_headers
                    .iter_mut()
                    .find(|row| row.doc_id == original_prompt.doc_id)
                    .unwrap()
                    .message
                    .node_did = "did:other".into()
            }
            _ => {}
        }
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![child.clone()],
            transcript_messages: page.clone(),
            ..Default::default()
        });
        let mut snapshot =
            super::super::projection::build_session_snapshot_from_store_for_node_with_transcript(
                &store,
                &store,
                &store,
                Some(&dependencies),
                None,
                true,
                true,
                true,
                false,
                Some(&child.node_did),
                "child",
                None,
                None,
            )
            .unwrap();
        let measured = crate::tests::support::CountingRead::new(access.clone());
        attach(&measured, &[], &page, Some(&dependencies), &mut snapshot)
            .await
            .unwrap();
        assert!(measured.queries() <= 2, "{fault} must batch origin reads");
        let markers = snapshot
            .messages
            .iter()
            .filter_map(|message| message.started_goal_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            markers,
            if fault == "none" {
                vec!["goal-0"]
            } else {
                vec![]
            },
            "{fault}"
        );
        let timeline = snapshot
            .timeline_items
            .iter()
            .filter_map(|item| match item {
                RenderedTimelineItem::UserMessage {
                    started_goal_id, ..
                } => started_goal_id.as_deref(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(timeline, markers, "{fault} timeline");
    }
}
