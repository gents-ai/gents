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
        stream_bytes_after: Some(vec![(CHUNK.len() * 2) as u64]),
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

#[tokio::test]
async fn per_append_source_reads_stay_bounded_as_segment_count_grows() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-bounded-reads".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();

    for index in 0..80usize {
        let ((result, payload_bytes), rows) = gents_session::session::count_source_rows_decoded(
            gents_session::session::count_source_payload_bytes(super::append_tool_output(
                &binding, CHUNK,
            )),
        )
        .await;
        let range = result.unwrap();
        assert_eq!(range.start, (index * CHUNK.len()) as u64);
        assert_eq!(range.end, ((index + 1) * CHUNK.len()) as u64);
        assert!(
            rows <= 3,
            "append #{index} decoded {rows} source rows; expected <= 3"
        );
        assert_eq!(
            payload_bytes, 0,
            "fresh append #{index} should not read payload bytes"
        );
    }

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
async fn corrupted_stream_bytes_after_is_rejected_on_append_and_close() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-corrupt-bytes-after".to_owned(),
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
    let access = binding_access(&binding);

    super::append_tool_output(&binding, CHUNK).await.unwrap();

    let corrupt = OutputSegment {
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
        stream_bytes_after: Some(vec![999_999]),
        payload: CHUNK.to_owned(),
        close: None,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    };
    access
        .transact_idempotent(
            gents_store::config_client::IdempotentTransactionRetry::Standard,
            "test.inject_corrupt_stream_bytes_after",
            move |txn| {
                let corrupt = corrupt.clone();
                Box::pin(async move {
                    let variables =
                        gents_store::config_client::session_children::output_segment_bound_variables(
                            txn, &corrupt,
                        )
                        .await?;
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

    assert!(
        super::append_tool_output(&binding, CHUNK).await.is_err(),
        "append must reject corrupted stream_bytes_after on the tail"
    );
    assert!(
        tool.complete(&CHUNK.repeat(2)).await.is_err(),
        "close must reject corrupted stream_bytes_after during full extent validation"
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
async fn regressed_timestamp_and_retry_of_earlier_attempt_follow_tail_contract() {
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "append-time-and-earlier-retry".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();

    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::milliseconds(10);
    let t2 = t0 + chrono::Duration::milliseconds(20);
    let r0 = super::append_tool_output_at(&binding, "first", t0, t0)
        .await
        .unwrap();
    super::append_tool_output_at(&binding, "second", t1, t1)
        .await
        .unwrap();
    super::append_tool_output_at(&binding, "third", t2, t2)
        .await
        .unwrap();

    let regressed = t0 + chrono::Duration::milliseconds(5);
    assert!(
        super::append_tool_output_at(&binding, "late", regressed, t2)
            .await
            .is_err(),
        "fresh append with timestamp earlier than tail must be rejected"
    );

    let replayed = super::append_tool_output_at(&binding, "first", t0, t2)
        .await
        .unwrap();
    assert_eq!(replayed.segment_doc_id, r0.segment_doc_id);
    assert_eq!(replayed.range, r0.range);

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}
