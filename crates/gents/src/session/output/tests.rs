use super::*;
use gents_protocol::output::{MessageRole, OutputOutcome, TranscriptMessage};

#[test]
fn session_header_index_preserves_coordinate_observations() {
    let rows = vec![
        serde_json::json!({"_docID": "duplicate", "message_key": "key", "sequence": 1}),
        serde_json::json!({"_docID": "duplicate", "message_key": "key", "sequence": 2}),
        serde_json::json!({"_docID": "sequence-twin", "message_key": "other", "sequence": 1}),
        serde_json::json!({"message_key": false, "sequence": -1, "blocks": "malformed"}),
        serde_json::json!({"message_key": "key", "sequence": "1", "blocks": "malformed"}),
        serde_json::json!({"message_key": 1, "sequence": 1, "blocks": "malformed"}),
    ];
    let indexed = SessionHeaderRows::from(rows.clone());
    for (key, sequence, expected) in [
        ("key", 1, vec![0, 1, 2, 4, 5]),
        ("other", 2, vec![1, 2]),
        ("missing", 3, vec![]),
    ] {
        assert_eq!(
            indexed
                .coordinate_matches(key, sequence)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|&index| &rows[index])
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn current_input_selection_matches_lean_owner() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().current_input_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let retained = case
            .headers
            .iter()
            .enumerate()
            .filter_map(|(index, input)| {
                let publication = match input.kind.as_str() {
                    "prompt" | "context" | "folded" | "assistant" => {
                        MessagePublication::RequestExecution {
                            execution_generation: "observed-generation".into(),
                        }
                    }
                    "tool_result" | "notification" => MessagePublication::ToolDelivery {
                        tool_call_doc_id: format!("tool-{index}"),
                    },
                    other => panic!("unsupported modeled header class {other}"),
                };
                let mut row = header(
                    &index.to_string(),
                    "session",
                    Some(&input.request),
                    publication,
                );
                let key = match input.kind.as_str() {
                    "folded" => crate::lifecycle::queue::folded_input_key("folded-request"),
                    kind => kind.to_owned(),
                };
                row.message.message_key =
                    super::super::canonical_rows::authored_message_key(&input.request, &key);
                (!is_current_admission_input(&row.message, &case.current_request)).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(retained, case.retained_indices, "{}", case.name);
    }
}

fn header(
    doc_id: &str,
    session_id: &str,
    request_doc_id: Option<&str>,
    publication: MessagePublication,
) -> super::super::canonical_rows::TranscriptMessageRow {
    super::super::canonical_rows::TranscriptMessageRow {
        doc_id: doc_id.into(),
        message: TranscriptMessage {
            message_key: format!("key-{doc_id}"),
            session_id: session_id.into(),
            node_did: "did:test:agent".into(),
            requester_did: Some("did:test:requester".into()),
            request_doc_id: request_doc_id.map(str::to_owned),
            publication,
            outcome: OutputOutcome::Complete,
            sequence: 7,
            role: MessageRole::Assistant,
            native_id: Some("native-message".into()),
            blocks: Vec::new(),
            created_at: "2026-09-21T00:00:00Z".into(),
        },
    }
}

async fn create_doc(
    node: &EmbeddedNode,
    mutation: &str,
    variables: serde_json::Value,
    field: &str,
) -> String {
    let response = node
        .execute_request(defra_node::QueryRequest::new(mutation).with_variables(variables))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    crate::graphql::single_mutation_document(&response, field)
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "manual canonical session-coordinate benchmark"]
fn session_coordinate_lookup_size_benchmark() {
    let mut measurements = Vec::new();
    for count in [1_usize, 500, 10_000] {
        let rows = (0 .. count) . map (| sequence | { serde_json :: json ! ({ "_docID" : format ! ("header-{sequence}") , "message_key" : format ! ("key-{sequence}") , "sequence" : sequence , }) }) . collect :: < Vec < _ > > () ;
        let coordinates = (0..count)
            .map(|sequence| (format!("key-{sequence}"), u32::try_from(sequence).unwrap()))
            .collect::<Vec<_>>();
        let mut baseline = Vec::new();
        let mut indexed = Vec::new();
        let mut construction = Vec::new();
        for iteration in 0..6 {
            let start = std::time::Instant::now();
            let cache = SessionHeaderRows::from(rows.clone());
            let build_ms = start.elapsed().as_secs_f64() * 1000.0;
            let scan = || {
                let start = std::time::Instant::now();
                for (key, sequence) in &coordinates {
                    let matches = rows
                        .iter()
                        .filter(|row| {
                            row.get("message_key").and_then(serde_json::Value::as_str)
                                == Some(key.as_str())
                                || row.get("sequence").and_then(serde_json::Value::as_u64)
                                    == Some(u64::from(*sequence))
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(std::hint::black_box(matches).len(), 1);
                }
                start.elapsed().as_secs_f64() * 1000.0
            };
            let lookup = || {
                let start = std::time::Instant::now();
                for (key, sequence) in &coordinates {
                    let matches = cache.coordinate_matches(key, *sequence).collect::<Vec<_>>();
                    assert_eq!(std::hint::black_box(matches).len(), 1);
                }
                start.elapsed().as_secs_f64() * 1000.0
            };
            let (scan_ms, lookup_ms) = if iteration % 2 == 0 {
                (scan(), lookup())
            } else {
                let lookup_ms = lookup();
                (scan(), lookup_ms)
            };
            if iteration > 0 {
                baseline.push(scan_ms);
                indexed.push(lookup_ms);
                construction.push(build_ms);
            }
        }
        for values in [&mut baseline, &mut indexed, &mut construction] {
            values.sort_by(f64::total_cmp);
        }
        measurements . push (serde_json :: json ! ({ "headers" : count , "lookups" : count , "baseline_median_ms" : baseline [2] , "indexed_median_ms" : indexed [2] , "construction_with_row_clone_median_ms" : construction [2] , })) ;
    }
    let result = serde_json :: json ! ({ "measurements" : measurements , "includes_database_reads" : false });
    if let Some(path) = std::env::var_os("GENTS_COORDINATE_BENCHMARK_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    tracing :: info ! (% result , "session coordinate lookup benchmark");
}

#[tokio::test]
#[ignore = "manual canonical request-size benchmark"]
async fn cached_output_reconstruction_request_size_benchmark() {
    use gents_protocol::output::{MessageBlock, PayloadPresentation, PresentedPayload};
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let node = EmbeddedNode::builder().build().await.unwrap();
    let request = "request-size-benchmark";
    let payload = "x".repeat(64 * 1024);
    for sources in [1, 32, 128] {
        let mut rows = Vec::new();
        let mut headers = Vec::new();
        for index in 0..sources {
            let key = format!("source-{index}");
            let doc_id = format!("close-{index}");
            let segment = authored_segments(request, &key, &[&payload]).pop().unwrap();
            let mut message = header(
                &format!("header-{index}"),
                &segment.session_id,
                Some(request),
                MessagePublication::RequestExecution {
                    execution_generation: "observed-generation".into(),
                },
            );
            message.message.node_did = segment.node_did.clone();
            message.message.requester_did = None;
            message.message.role = MessageRole::User;
            message.message.native_id = None;
            message.message.sequence = index;
            message.message.blocks = vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: doc_id.clone(),
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }];
            rows.push(super::super::canonical_rows::OutputSegmentRow { doc_id, segment });
            headers.push(message);
        }
        let mut cache = ReadCache::default();
        cache.requests.insert(request.into(), Arc::new(rows.into()));
        let expected = gents_protocol::message::Message::user(&payload);
        let mut samples = Vec::new();
        for pass in 0..6 {
            let started = std::time::Instant::now();
            for message in &headers {
                let selected = load_referenced_segments(
                    ReadAccess::Node(&node),
                    message,
                    Some(request),
                    "did:test:test",
                    None,
                    &mut cache,
                )
                .await
                .unwrap();
                let observed = selected
                    .iter()
                    .map(|row| ObservedSegment {
                        doc_id: &row.doc_id,
                        segment: &row.segment,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    reconstruct_message(&observed, &[], &[], &message.message).unwrap(),
                    expected
                );
            }
            if pass > 0 {
                samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            }
        }
        samples.sort_by(f64::total_cmp);
        tracing::info!(
            sources,
            payload_bytes = payload.len(),
            median_ms = samples[2],
            min_ms = samples[0],
            max_ms = samples[4],
            "cached canonical reconstruction benchmark"
        );
    }
    node.shutdown().await;
}

fn authored_segments(
    request_doc_id: &str,
    key: &str,
    parts: &[&str],
) -> Vec<gents_protocol::output::OutputSegment> {
    use gents_protocol::output::*;
    let total = parts.iter().map(|part| part.len() as u64).sum::<u64>();
    parts
        .iter()
        .enumerate()
        .map(|(ordinal, part)| OutputSegment {
            node_did: "did:test:test".into(),
            requester_did: None,
            session_id: "session-scan-gap".into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::Authored { key: key.into() },
            writer: OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
            ordinal: Some(ordinal as u32),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: part.len() as u32,
                declaration: (ordinal == 0).then_some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: (*part).into(),
            close: (ordinal + 1 == parts.len()).then(|| SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: parts.len() as u32,
                stream_bytes: vec![total],
            }),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .collect()
}

async fn create_authored_header(
    node: &EmbeddedNode,
    request_doc_id: &str,
    key: &str,
    sequence: u32,
    close_doc_id: String,
) -> String {
    use gents_protocol::output::*;
    let header = TranscriptMessage {
        message_key: key.into(),
        session_id: "session-scan-gap".into(),
        node_did: "did:test:test".into(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "observed-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    create_doc(
        node,
        super::super::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
        super::super::canonical_rows::transcript_message_create_variables(&header).unwrap(),
        "create_AgentMessage",
    )
    .await
}

#[tokio::test]
async fn reused_scan_observes_payload_segments_replicated_after_their_close() {
    use super::super::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    let node = crate::test_support::node_builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let request = "doc-scan-gap";
    let create_segment = |segment: gents_protocol::output::OutputSegment| {
        let node = &node;
        async move {
            create_doc(
                node,
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                output_segment_create_variables(&segment).unwrap(),
                "create_AgentOutputSegment",
            )
            .await
        }
    };
    let [a] = authored_segments(request, "a", &["first"])
        .try_into()
        .unwrap();
    let a_close = create_segment(a).await;
    let a_header = create_authored_header(&node, request, "a", 1, a_close).await;
    let [b_payload, b_close] = authored_segments(request, "b", &["hel", "lo"])
        .try_into()
        .unwrap();
    let b_close = create_segment(b_close).await;
    let b_header = create_authored_header(&node, request, "b", 2, b_close).await;

    let mut cache = ReadCache {
        reuse_node_scans: true,
        ..ReadCache::default()
    };
    let a = reconstruct_scoped_message(
        ReadAccess::Node(&node),
        &a_header,
        "did:test:test",
        None,
        &mut cache,
    )
    .await
    .unwrap();
    assert_eq!(a.1, gents_protocol::message::Message::user("first"));

    create_segment(b_payload).await;
    let b = reconstruct_scoped_message(
        ReadAccess::Node(&node),
        &b_header,
        "did:test:test",
        None,
        &mut cache,
    )
    .await
    .unwrap();
    assert_eq!(b.1, gents_protocol::message::Message::user("hello"));
}
