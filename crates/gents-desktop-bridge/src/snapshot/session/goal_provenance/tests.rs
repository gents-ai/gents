use super::*;

pub(crate) async fn assert_copied_opening(
    access: &gents::config_client::ConfigAccess,
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
        attach(&measured, &[], &page, Some(&dependencies), &mut snapshot).await;
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
