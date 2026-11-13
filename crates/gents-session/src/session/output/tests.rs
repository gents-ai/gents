use defra_node::Node as EmbeddedNode;
use gents_protocol::output::MessagePublication;

use super::access::{ReadAccess, ReadCache, SessionHeaderRows};
use super::reconstruct::reconstruct_scoped_message;
use super::sequenced::is_current_admission_input;
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
    let cases = &gents_lean_vocab::lean_contract_snapshot().current_input_cases;
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
                    "folded" => {
                        crate::request::lifecycle::queue::folded_input_key("folded-request")
                    }
                    kind => kind.to_owned(),
                };
                row.message.message_key =
                    gents_protocol::output::rows::authored_message_key(&input.request, &key);
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
) -> gents_protocol::output::rows::TranscriptMessageRow {
    gents_protocol::output::rows::TranscriptMessageRow {
        doc_id: doc_id.into(),
        message: TranscriptMessage {
            native_request_id: None,
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
    gents_store::graphql::single_mutation_document(&response, field)
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
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
        native_request_id: None,
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
        gents_protocol::output::rows::CREATE_AGENT_MESSAGE_MUTATION,
        gents_protocol::output::rows::transcript_message_create_variables(&header).unwrap(),
        "create_AgentMessage",
    )
    .await
}

#[tokio::test]
async fn late_delivered_segments_match_the_lean_reads() {
    use gents_protocol::output::rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    for case in gents_lean_vocab::families::late_delivery_reads::cases::<String>() {
        let db = gents_inference::test_support::test_db(
            "late-delivered-segments-match-the-lean-r",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        let request = "doc-scan-gap";
        let mut headers = vec![None; case.input.messages.len()];
        let mut cache = ReadCache {
            reuse_node_scans: true,
            ..ReadCache::default()
        };
        let mut reads = Vec::new();
        for step in &case.input.steps {
            let index = step.message as usize;
            let message = &case.input.messages[index];
            let key = format!("key-{}", message.key);
            let parts = message.parts.iter().map(String::as_str).collect::<Vec<_>>();
            match step.kind.as_str() {
                "deliver" => {
                    let segment =
                        authored_segments(request, &key, &parts).swap_remove(step.ordinal as usize);
                    let doc = create_doc(
                        &node,
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        output_segment_create_variables(&segment).unwrap(),
                        "create_AgentOutputSegment",
                    )
                    .await;
                    if step.ordinal as usize + 1 == parts.len() {
                        headers[index] = Some(
                            create_authored_header(&node, request, &key, index as u32 + 1, doc)
                                .await,
                        );
                    }
                }
                "read" => {
                    let header = headers[index].as_deref().expect("read before its closure");
                    let read = reconstruct_scoped_message(
                        ReadAccess::Node(&node),
                        header,
                        "did:test:test",
                        None,
                        &mut cache,
                    )
                    .await
                    .ok()
                    .map(|(_, message)| match message {
                        gents_protocol::message::Message::User { content } => {
                            gents_lean_vocab::families::late_delivery_reads::ReadWire {
                                role: "user".to_owned(),
                                blocks: content
                                    .into_iter()
                                    .map(|item| match item {
                                        gents_protocol::message::UserContent::Text(text) => {
                                            text.text
                                        }
                                        _ => "non-text".to_owned(),
                                    })
                                    .collect(),
                            }
                        }
                        other => panic!("authored user header reconstructed as {other:?}"),
                    });
                    reads.push(read);
                }
                other => panic!("unmodeled step {other}"),
            }
        }
        case.check(reads);
    }
}
