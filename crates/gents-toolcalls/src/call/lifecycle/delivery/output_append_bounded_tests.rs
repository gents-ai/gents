use crate::call::lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use gents_protocol::output::{
    OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose,
};
const CHUNK: &str = "0123456789abcdef";

fn binding_access(
    binding: &super::output_append::ToolOutputBinding,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_with_optional_agent_signer(
        binding.node.clone(),
        binding.actor.clone(),
        binding.agent_signer.clone(),
    )
    .unwrap()
}

async fn source_rows_with_access(
    access: &gents_store::config_client::ConfigAccess,
    binding: &super::output_append::ToolOutputBinding,
) -> Vec<gents_protocol::output::rows::OutputSegmentRow> {
    let source = OutputSource::ToolCall {
        tool_call_doc_id: binding.tool_call_doc_id.clone(),
    };
    let response = access
        .execute(
            &gents_protocol::output::rows::source_output_segments_query(
                &binding.request_doc_id,
                &source,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let values = response["data"]["AgentOutputSegment"].as_array().unwrap();
    gents_protocol::output::rows::decode_scoped_request_output_segments(
        values,
        &binding.node_did,
        Some(&binding.session_id),
        binding.requester_did.as_deref(),
    )
    .unwrap()
    .into_iter()
    .filter(|row| row.segment.source == source)
    .collect()
}

#[tokio::test]
async fn appends_tile_byte_ranges_and_close_commits_the_extent() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-tiling".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node,
        path,
        mut tool,
        ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();

    let mut next = 0u64;
    for _ in 0..80u32 {
        let range = super::append_tool_output(&binding, CHUNK).await.unwrap();
        assert_eq!(range.start, next, "append ranges must tile without a gap");
        assert_eq!(range.end, next + CHUNK.len() as u64);
        next = range.end;
    }
    let text = CHUNK.repeat(80);
    assert_eq!(next, text.len() as u64);

    let access = binding_access(&binding);
    let observed =
        gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
            &access,
            &binding.tool_call_doc_id,
            &binding.request_doc_id,
            &binding.session_id,
            &binding.node_did,
            binding.requester_did.as_deref(),
        )
        .await
        .unwrap();
    assert_eq!(observed.into_text(), text);

    tool.complete(&text).await.unwrap();
    let closes = source_rows_with_access(&access, &binding)
        .await
        .into_iter()
        .filter_map(|row| row.segment.close)
        .collect::<Vec<_>>();
    assert_eq!(
        closes,
        vec![SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 80,
            stream_bytes: vec![text.len() as u64],
        }]
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
async fn retry_identity_with_different_bytes_is_rejected() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-retry-conflict".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();

    let now = chrono::Utc::now();
    super::append_tool_output_at(&binding, "first chunk", now, now)
        .await
        .unwrap();
    let error = super::append_tool_output_at(&binding, "other chunk", now, now)
        .await
        .expect_err("one attempt identity cannot record two different segments");
    assert!(
        format!("{error:#}").contains("retry conflicts with the recorded segment"),
        "{error:#}"
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
async fn idempotent_retry_returns_the_recorded_range_without_a_new_row() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-idempotent-retry".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();
    let access = binding_access(&binding);

    let first = chrono::Utc::now();
    super::append_tool_output_at(&binding, "first chunk", first, first)
        .await
        .unwrap();
    let second = first + chrono::Duration::milliseconds(1);
    let recorded = super::append_tool_output_at(&binding, "second chunk", second, second)
        .await
        .unwrap();
    let rows_before = source_rows_with_access(&access, &binding).await.len();

    let replayed = super::append_tool_output_at(&binding, "second chunk", second, second)
        .await
        .unwrap();
    assert_eq!(replayed.range, recorded.range);
    assert_eq!(replayed.segment_doc_id, recorded.segment_doc_id);
    assert_eq!(
        source_rows_with_access(&access, &binding).await.len(),
        rows_before,
        "a replayed attempt must not write a second row"
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
async fn conflicting_ordinal_twin_rejects_the_next_append() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-ordinal-twin".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();
    let access = binding_access(&binding);

    super::append_tool_output(&binding, CHUNK).await.unwrap();
    super::append_tool_output(&binding, CHUNK).await.unwrap();

    let twin = OutputSegment {
        node_did: binding.node_did.clone(),
        requester_did: binding.requester_did.clone(),
        session_id: binding.session_id.clone(),
        request_doc_id: binding.request_doc_id.clone(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: binding.tool_call_doc_id.clone(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: binding.tool_call_doc_id.clone(),
        },
        ordinal: Some(1),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: CHUNK.len() as u32,
            declaration: None,
        }],
        payload: CHUNK.chars().rev().collect(),
        close: None,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    };
    access
        .transact_idempotent(
            gents_store::config_client::IdempotentTransactionRetry::Standard,
            "test.inject_conflicting_ordinal_twin",
            move |txn| {
                let twin = twin.clone();
                Box::pin(async move {
                    let variables = gents_store::config_client::session_children::output_segment_bound_variables(txn, &twin).await?;
                    txn.execute_with_variables(
                        gents_protocol::output::rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &variables,
                    )
                    .await?;
                    anyhow::Ok(())
                })
            },
        )
        .await
        .unwrap();

    let error = super::append_tool_output(&binding, CHUNK)
        .await
        .expect_err("an append cannot extend a source with conflicting evidence");
    assert!(
        format!("{error:#}").contains("conflicting flush twin"),
        "{error:#}"
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}
