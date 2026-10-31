use super::*;
use crate::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::*;

#[tokio::test]
async fn transaction_payload_reads_cross_pages_and_reject_conflicting_segments() {
    let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
        .await
        .unwrap();
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let did = node.node_identity_did().unwrap().to_owned();
    let source = OutputSource::Authored {
        key: "paged-canonical".into(),
    };
    let segment = OutputSegment {
        node_did: did.clone(),
        requester_did: None,
        session_id: "paged-canonical-session".into(),
        request_doc_id: "paged-canonical-request".into(),
        source: source.clone(),
        writer: OutputWriter::RequestExecution {
            execution_generation: "paged-generation".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 1,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: "x".into(),
        close: None,
        created_at: "2026-10-24T00:00:00Z".into(),
    };
    let (header_id, close_id) =
        ConfigAccess::transact_local_write(&node, None, "test.output.seed_pages", |txn| {
            let segment = &segment;
            Box::pin(async move {
                let mut close_id = String::new();
                for ordinal in 0..129 {
                    let mut row = segment.clone();
                    row.ordinal = Some(ordinal);
                    if ordinal != 0 {
                        row.runs[0].declaration = None;
                    }
                    if ordinal == 128 {
                        row.close = Some(SourceClose::Closed {
                            outcome: OutputOutcome::Complete,
                            segments: 129,
                            stream_bytes: vec![129],
                        });
                    }
                    let response = txn
                        .execute_with_variables(
                            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                            &output_segment_create_variables(&row)?,
                        )
                        .await?;
                    close_id = crate::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                }
                let header = TranscriptMessage {
                    native_request_id: None,
                    message_key: "paged-canonical".into(),
                    session_id: segment.session_id.clone(),
                    node_did: segment.node_did.clone(),
                    requester_did: None,
                    request_doc_id: Some(segment.request_doc_id.clone()),
                    publication: MessagePublication::RequestExecution {
                        execution_generation: "paged-generation".into(),
                    },
                    outcome: OutputOutcome::Complete,
                    sequence: 1,
                    role: MessageRole::User,
                    native_id: None,
                    blocks: vec![MessageBlock::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: close_id.clone(),
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                    created_at: segment.created_at.clone(),
                };
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_MESSAGE_MUTATION,
                        &transcript_message_create_variables(&header)?,
                    )
                    .await?;
                Ok::<_, anyhow::Error>((
                    crate::graphql::created_doc_id(&response, "AgentMessage")?,
                    close_id,
                ))
            })
        })
        .await
        .unwrap();
    let reference = PayloadRef {
        close_doc_id: close_id,
        stream: 0,
    };
    ConfigAccess::transact_local_readonly(&node, None, "test.output.read_pages", |txn| {
        let (header_id, reference, did, source) = (&header_id, &reference, &did, &source);
        Box::pin(async move {
            let (_, message) = load_canonical_message_in_txn(txn, header_id, did, None).await?;
            assert_eq!(
                message,
                gents_protocol::message::Message::User {
                    content: vec![gents_protocol::message::UserContent::Text(
                        gents_protocol::message::Text {
                            text: "x".repeat(129)
                        }
                    )]
                }
            );
            let payload = load_canonical_payload_in_txn(
                txn,
                "paged-canonical-request",
                did,
                None,
                reference,
                source,
            )
            .await?;
            assert_eq!(payload.text, "x".repeat(129));
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .unwrap();
    ConfigAccess::transact_local_write(&node, None, "test.output.conflicting_segment", |txn| {
        let mut conflict = segment.clone();
        conflict.payload = "y".into();
        Box::pin(async move {
            txn.execute_with_variables(
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                &output_segment_create_variables(&conflict)?,
            )
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
        })
    })
    .await
    .unwrap();
    ConfigAccess::transact_local_readonly(&node, None, "test.output.reject_conflict", |txn| {
        let (header_id, reference, did, source) = (&header_id, &reference, &did, &source);
        Box::pin(async move {
            assert!(load_canonical_message_in_txn(txn, header_id, did, None)
                .await
                .is_err());
            assert!(load_canonical_payload_in_txn(
                txn,
                "paged-canonical-request",
                did,
                None,
                reference,
                source
            )
            .await
            .is_err());
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .unwrap();
    node.shutdown().await;
}
