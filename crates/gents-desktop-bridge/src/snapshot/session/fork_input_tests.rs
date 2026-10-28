use super::*;
use crate::snapshot::tests::push_canonical_text_message;
use gents_desktop_core::client::{CanonicalTranscriptDependencies, ClientStoreRows};
use gents_protocol::output::{MessagePublication, MessageRole};

fn fork_fixture() -> (ClientStore, CanonicalTranscriptDependencies) {
    let request = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("source-request".into()),
        request_id: "source-logical-request".into(),
        node_did: Some("did:test:amy".into()),
        session_id: Some("source".into()),
        input: Some(
            serde_json::from_value(serde_json::json!({
                "queue": {"source": "goal", "policy": "append"},
                "goal_continuation": {"sequence": 3, "wrapup": false}
            }))
            .unwrap(),
        ),
        caused_by_trigger_id: Some("goal".into()),
        ..Default::default()
    };
    let mut rows = ClientStoreRows::default();
    push_canonical_text_message(
        &mut rows,
        "original",
        "source",
        Some("source-request"),
        1,
        MessageRole::User,
        "controller input",
    );
    let original = rows.transcript_messages[0].clone();
    let mut copied = original.clone();
    copied.doc_id = "first-copy".into();
    copied.message.session_id = "first".into();
    copied.message.message_key = "first-key".into();
    copied.message.request_doc_id = None;
    copied.message.publication = MessagePublication::Fork {
        origin_message_doc_id: original.doc_id.clone(),
    };
    let mut nested = copied.clone();
    nested.doc_id = "second-copy".into();
    nested.message.session_id = "second".into();
    nested.message.message_key = "second-key".into();
    nested.message.publication = MessagePublication::Fork {
        origin_message_doc_id: copied.doc_id.clone(),
    };
    let dependencies = CanonicalTranscriptDependencies {
        origin_headers: vec![original, copied],
        origin_requests: vec![request],
        output_segments: rows.output_segments,
        ..Default::default()
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        transcript_messages: vec![nested],
        ..Default::default()
    });
    (store, dependencies)
}

#[test]
fn nested_fork_keeps_automated_input_origin_without_child_turn_membership() {
    let (store, dependencies) = fork_fixture();
    let (messages, _) = project_transcript_rows(
        &store,
        &store.transcript("second"),
        Some(&dependencies),
        &[],
        &RapidHashMap::default(),
        &[],
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].reconstruction_state, ReconstructionState::Ready);
    assert!(matches!(&messages[0].origin,
        Some(RequestOriginView::GoalContinuation { goal_id: Some(id), sequence: Some(3) }) if id == "goal"));
    assert!(messages[0].request_id.is_none());
    assert!(messages[0].input_request_id.is_none());
    assert!(!messages[0].owns_turn);
}

#[tokio::test]
async fn real_fork_reads_exact_input_metadata_and_preserves_provider_history() {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    let node = std::sync::Arc::new(crate::tests::support::node_builder().build().await.unwrap());
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    for mutation in [
        r#"mutation { create_Agent(input: {node_did:"did:test:amy", agent_id:"agent", inference_profile_id:"profile", enabled:true}) { _docID } }"#,
        r#"mutation { create_AgentSession(input: {node_did:"did:test:amy", agent_id:"agent", session_id:"source", created_at:"2026-10-26T00:00:00Z"}) { _docID } }"#,
    ] {
        ConfigAccess::write_local(&node, "test.fork_input", mutation)
            .await
            .unwrap();
    }
    let response = ConfigAccess::write_local(&node, "test.fork_input", r#"mutation {
        create_AgentRequest(input: {purpose:"normal", node_did:"did:test:amy", agent_id:"agent",
            request_id:"goal-continuation", session_id:"source", lifecycle_state:"completed",
            caused_by_trigger_kind:"goal", caused_by_trigger_id:"goal",
            input:{queue:{source:"goal",policy:"coalesce",key:"goal:source"},goal_continuation:{sequence:3,wrapup:false}},
            created_at:"2026-10-26T00:00:00Z"}) { _docID }
    }"#).await.unwrap();
    let request_doc = gents::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let mut rows = ClientStoreRows::default();
    push_canonical_text_message(
        &mut rows,
        "original",
        "source",
        Some(&request_doc),
        1,
        MessageRole::User,
        "Continue the durable Goal, preserving its budget.",
    );
    let header = rows.transcript_messages.pop().unwrap().message;
    let segment = rows.output_segments.pop().unwrap().segment;
    ConfigAccess::transact_local_write(&node, None, "test.fork_input_output", |txn| {
        let mut header = header.clone();
        let segment = segment.clone();
        Box::pin(async move {
            let response = txn
                .execute_with_variables(
                    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                    &output_segment_create_variables(&segment)?,
                )
                .await?;
            let close = gents::graphql::created_doc_id(&response, "AgentOutputSegment")?;
            if let gents_protocol::output::MessageBlock::Text { text } = &mut header.blocks[0] {
                text.output.close_doc_id = close;
            }
            txn.execute_with_variables(
                CREATE_AGENT_MESSAGE_MUTATION,
                &transcript_message_create_variables(&header)?,
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .unwrap();
    let original = gents::session::load_history(&node, "source", "did:test:amy", None)
        .await
        .unwrap();
    assert_eq!(original.len(), 1);
    let mut source = "source".to_owned();
    for _ in 0..2 {
        let fork = gents::session::fork_on(
            &access,
            gents::session::ForkParams {
                source_session_id: &source,
                boundary: gents::session::ForkBoundary::UserTurn(1),
                caller_node_did: "did:test:amy",
                caller_requester_did: None,
                target_agent_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(fork.copied_messages, 1);
        source = fork.session_id;
    }
    let page = gents_desktop_core::client::load_session_transcript_page_on(
        &access,
        &source,
        Some("did:test:amy"),
        None,
        None,
        Some(10),
    )
    .await
    .unwrap();
    assert_eq!(page.canonical_dependencies.origin_headers.len(), 2);
    assert_eq!(page.canonical_dependencies.origin_requests.len(), 1);
    assert_eq!(
        page.canonical_dependencies.origin_requests[0]
            .doc_id
            .as_deref(),
        Some(request_doc.as_str())
    );
    assert!(page.requests.is_empty());
    assert!(page.store.requests.is_empty());
    let (messages, _) = project_transcript_rows(
        &page.store,
        &page.store.transcript(&source),
        Some(&page.canonical_dependencies),
        &[],
        &RapidHashMap::default(),
        &[],
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].reconstruction_state, ReconstructionState::Ready);
    assert!(matches!(
        messages[0].origin,
        Some(RequestOriginView::GoalContinuation {
            sequence: Some(3),
            ..
        })
    ));
    assert!(messages[0].runtime_control);
    assert!(messages[0].request_turn.is_none());
    let copied = gents::session::load_history(&node, &source, "did:test:amy", None)
        .await
        .unwrap();
    assert_eq!(copied, original);
    node.shutdown().await;
}

#[test]
fn fork_input_metadata_requires_exact_source_scope() {
    for fault in ["missing", "node", "requester", "session"] {
        let (store, mut dependencies) = fork_fixture();
        match fault {
            "missing" => dependencies.origin_requests.clear(),
            "node" => dependencies.origin_requests[0].node_did = Some("foreign".into()),
            "requester" => dependencies.origin_requests[0].requester_did = Some("foreign".into()),
            "session" => dependencies.origin_requests[0].session_id = Some("foreign".into()),
            _ => unreachable!(),
        }
        let (messages, _) = project_transcript_rows(
            &store,
            &store.transcript("second"),
            Some(&dependencies),
            &[],
            &RapidHashMap::default(),
            &[],
        );
        assert_eq!(
            messages[0].reconstruction_state,
            ReconstructionState::Loading,
            "{fault}"
        );
        assert!(messages[0].display_content.is_none(), "{fault}");
        assert!(messages[0].origin.is_none(), "{fault}");
    }
}

#[test]
fn copied_person_and_automated_input_keep_the_source_presentation() {
    for kind in ["person", "goal", "trigger", "background"] {
        let (store, mut dependencies) = fork_fixture();
        let request = &mut dependencies.origin_requests[0];
        match kind {
            "person" => {
                request.input = None;
                request.caused_by_trigger_id = None;
            }
            "trigger" => {
                request.input = None;
                request.caused_by_trigger_kind = Some("schedule".into());
            }
            "background" => {
                request.input = Some(serde_json::from_value(serde_json::json!({
                "queue": {"source": "background_completion", "policy": "coalesce", "key": "wake"}
            })).unwrap());
            }
            _ => {}
        }
        let original_store = ClientStore::from_rows(ClientStoreRows {
            requests: dependencies.origin_requests.clone(),
            transcript_messages: vec![dependencies.origin_headers[0].clone()],
            output_segments: dependencies.output_segments.clone(),
            ..Default::default()
        });
        let request = &original_store.requests[0];
        let refs = [request];
        let by_doc = [(request.doc_id.as_deref().unwrap(), request)]
            .into_iter()
            .collect();
        let (original, _) = project_transcript_rows(
            &original_store,
            &original_store.transcript("source"),
            None,
            &refs,
            &by_doc,
            &[],
        );
        let (copied, _) = project_transcript_rows(
            &store,
            &store.transcript("second"),
            Some(&dependencies),
            &[],
            &RapidHashMap::default(),
            &[],
        );
        assert_eq!(copied[0].origin, original[0].origin, "{kind}");
        assert_eq!(
            copied[0].runtime_control, original[0].runtime_control,
            "{kind}"
        );
        assert_eq!(
            copied[0].display_content, original[0].display_content,
            "{kind}"
        );
        assert_eq!(
            copied[0].reconstruction_state,
            ReconstructionState::Ready,
            "{kind}"
        );
        assert!(copied[0].request_turn.is_none(), "{kind}");
    }
}
