use super::*;
use gents_protocol::output::MessagePublication;

pub(crate) async fn assert_paged_native_forks(
    access: &gents::store::config_client::ConfigAccess,
    opening: &gents_desktop_core::client::ClientStoreRows,
    followup: &gents_protocol::row::AgentRequestRow,
) {
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents::session::{ForkBoundary, ForkParams};

    let session = &opening.sessions[0];
    let node = gents::store::graphql::escape_graphql_string(&session.node_did);
    access.write("goal_marker.paged_session", &format!(r#"mutation {{
        create_Agent(input: {{node_did:"{node}",agent_id:"default",inference_profile_id:"fixture",enabled:true}}) {{_docID}}
        create_AgentSession(input: {{node_did:"{node}",requester_did:"{node}",agent_id:"default",session_id:"session",created_at:"2026-10-25T00:00:00Z"}}) {{_docID}}
    }}"#)).await.unwrap();
    let mut publications = opening
        .transcript_messages
        .iter()
        .zip(&opening.output_segments)
        .filter(|(header, _)| !header.message.message_key.ends_with(":context"))
        .map(|(header, segment)| (header.message.clone(), segment.segment.clone()))
        .collect::<Vec<_>>();
    let source_prompt = publications[0].0.message_key.clone();
    let mut followup_prompt = String::new();
    for index in 0..4_u32 {
        for (offset, original) in publications[..2].to_vec().iter().enumerate() {
            let (mut header, mut segment) = original.clone();
            let key = format!("ordinary-{index}-{offset}");
            if index == 0 && offset == 0 {
                followup_prompt = key.clone();
            }
            header.message_key = key.clone();
            header.sequence = 4 + index * 2 + u32::try_from(offset).unwrap();
            header.request_doc_id = followup.doc_id.clone();
            segment.request_doc_id = followup.doc_id.clone().unwrap();
            segment.source = gents_protocol::output::OutputSource::Authored { key };
            publications.push((header, segment));
        }
    }
    access
        .transact_write("goal_marker.persist_timeline", |txn| {
            let publications = publications.clone();
            let node = node.clone();
            Box::pin(async move {
                let mut terminals = std::collections::BTreeMap::new();
                for (mut header, segment) in publications {
                    let response = txn
                        .execute_with_variables(
                            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                            &output_segment_create_variables(&segment)?,
                        )
                        .await?;
                    let close = gents::store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                    let gents_protocol::output::MessageBlock::Text { text } = &mut header.blocks[0]
                    else {
                        panic!("the Goal marker fixture publishes text messages");
                    };
                    text.output.close_doc_id = close;
                    let response = txn.execute_with_variables(
                        CREATE_AGENT_MESSAGE_MUTATION,
                        &transcript_message_create_variables(&header)?,
                    )
                    .await?;
                    terminals.insert(
                        header.request_doc_id.unwrap(),
                        gents::store::graphql::created_doc_id(&response, "AgentMessage")?,
                    );
                }
                txn.execute_with_variables(
                    &format!(r#"mutation($input: AgentRequestMutationInputArg!) {{
                        update_AgentRequest(filter: {{node_did: {{_eq: "{node}"}}, session_id: {{_eq: "session"}}}}, input: $input) {{_docID}}
                    }}"#),
                    &serde_json::json!({"input": {
                        "lifecycle_state": "completed",
                        "terminal_output": gents_protocol::output::TerminalOutput::NoMessage,
                    }}),
                ).await?;
                for (request, message) in terminals {
                    txn.execute_with_variables(
                        &format!(r#"mutation($input: AgentRequestMutationInputArg!) {{
                            update_AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, input: $input) {{_docID}}
                        }}"#, gents::store::graphql::escape_graphql_string(&request)),
                        &serde_json::json!({"input": {"terminal_output":
                            gents_protocol::output::TerminalOutput::Message { message_doc_id: message }
                        }}),
                    ).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();

    assert_native_pages(access, session, "session", 10, Some(&source_prompt)).await;
    let first = gents::session::fork_on(
        access,
        ForkParams {
            source_session_id: "session",
            boundary: ForkBoundary::UserTurn(3),
            caller_node_did: &session.node_did,
            caller_requester_did: session.requester_did.as_deref(),
            target_agent_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.copied_messages, 6);
    assert_native_pages(access, session, &first.session_id, 6, Some(&source_prompt)).await;
    let second = gents::session::fork_on(
        access,
        ForkParams {
            source_session_id: &first.session_id,
            boundary: ForkBoundary::UserTurn(2),
            caller_node_did: &session.node_did,
            caller_requester_did: session.requester_did.as_deref(),
            target_agent_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.copied_messages, 4);
    assert_native_pages(access, session, &second.session_id, 4, Some(&source_prompt)).await;
    for (key, child, count, marked) in [
        (followup_prompt.as_str(), "edit-followup", 2, true),
        (source_prompt.as_str(), "edit-opening", 0, false),
    ] {
        let copied = gents::session::fork_edit_on(
            access,
            ForkParams {
                source_session_id: "session",
                boundary: ForkBoundary::BeforeMessageKey(key),
                caller_node_did: &session.node_did,
                caller_requester_did: session.requester_did.as_deref(),
                target_agent_id: None,
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
            count,
            marked.then_some(source_prompt.as_str()),
        )
        .await;
    }
}

async fn assert_native_pages(
    access: &gents::store::config_client::ConfigAccess,
    source_session: &gents_protocol::session::AgentSession,
    session_id: &str,
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
                gents::goal::load_applied_opening_goal_id(access, request)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("goal-0")
            );
        }
        let rows = super::super::render_transcript_rows_with_provenance(
            access,
            &ClientStore::default(),
            &page,
            &source_session.node_did,
            session_id,
        )
        .await;
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
        assert_eq!(goal, "goal-0");
        if session_id == "session" {
            assert_eq!(Some(key.as_str()), opening_key);
        }
    }
}

pub(crate) async fn assert_copied_opening(
    access: &gents::store::config_client::ConfigAccess,
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
