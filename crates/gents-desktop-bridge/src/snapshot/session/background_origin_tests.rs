use super::*;
use crate::snapshot::tests::push_canonical_text_message;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::{ClientStoreRows, TranscriptWindow};
use gents_protocol::output::{MessageBlock, MessageRole};

#[tokio::test]
async fn persisted_background_inputs_keep_origin_across_transcript_windows() {
    let node = std::sync::Arc::new(crate::tests::support::node_builder().build().await.unwrap());
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let mut requests = Vec::new();
    for (id, input) in [
        (
            "wake",
            r#"{queue:{source:"background_completion",policy:"coalesce",key:"background_completion:origin",background_completion_wake_version:1}}"#,
        ),
        ("person", "null"),
    ] {
        let query = format!(
            r#"mutation {{ create_AgentRequest(input: {{purpose:"normal", node_did:"did:test:amy", agent_id:"agent", request_id:"{id}", session_id:"origin", lifecycle_state:"completed", input:{input}, created_at:"2026-10-26T00:00:00Z"}}) {{ _docID }} }}"#
        );
        let response = ConfigAccess::write_local(&node, "test.background_origin", &query)
            .await
            .unwrap();
        requests.push(gents::store::graphql::created_doc_id(&response, "AgentRequest").unwrap());
    }
    let wake = gents::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
    let success = "<tool-completion tool_call_id=\"spawned:one\" status=\"completed\"><result>done</result></tool-completion>";
    let failure = "<tool-completion tool_call_id=\"spawned:two\" status=\"failed\"><result>failed</result></tool-completion>";
    let messages = [
        (
            "background-completion-notification:tool-one:tool".to_owned(),
            &requests[0],
            success,
        ),
        (
            "background-completion-notification:tool-two:tool".to_owned(),
            &requests[0],
            failure,
        ),
        (
            format!("authored:{}:prompt", requests[0]),
            &requests[0],
            wake,
        ),
        (
            format!("authored:{}:prompt", requests[1]),
            &requests[1],
            wake,
        ),
    ];
    for (index, (key, request, content)) in messages.iter().enumerate() {
        let mut rows = ClientStoreRows::default();
        push_canonical_text_message(
            &mut rows,
            key,
            "origin",
            Some(request),
            (index + 1) as u32,
            MessageRole::User,
            content,
        );
        let header = rows.transcript_messages.pop().unwrap().message;
        let segment = rows.output_segments.pop().unwrap().segment;
        ConfigAccess::transact_local_write(&node, None, "test.background_origin_output", |txn| {
            let mut header = header.clone();
            let segment = segment.clone();
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&segment)?,
                    )
                    .await?;
                let close = gents::store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                if let MessageBlock::Text { text } = &mut header.blocks[0] {
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
    }
    for window in [TranscriptWindow::Tip, TranscriptWindow::Before(5)] {
        let page = gents_desktop_core::client::load_session_transcript_window_on(
            &access,
            "origin",
            Some("did:test:amy"),
            None,
            window,
            Some(20),
        )
        .await
        .unwrap();
        let items = render_transcript_rows(&ClientStore::default(), &page, "origin");
        assert_eq!(items.len(), 4);
        for (item, expected) in items.iter().zip([success, failure, wake]) {
            assert!(matches!(item, RenderedTimelineItem::AutomatedInput {
                origin: RequestOriginView::BackgroundCompletion, content: Some(content), ..
            } if content == expected));
        }
        assert!(
            matches!(&items[3], RenderedTimelineItem::UserMessage { content: Some(content), .. } if content == wake)
        );
    }
    node.shutdown().await;
}
