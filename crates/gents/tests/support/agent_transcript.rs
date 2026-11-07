//! Agent-authored transcript fixtures. The fixture Agent writes its own
//! session children (closing segments, headers, tool rows, compaction
//! cursors) with the bound fields the canonical writer adds, under one real
//! completed request in an Agent-owned session.

use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::*;

use super::agent_requests::{canonical_now, RequestActors, RequestSeed};

/// A source session owned by the fixture Agent, scoped to that Agent as
/// requester, whose rows name one real completed request.
pub struct Source {
    pub session: String,
    pub requester: String,
    pub session_doc: String,
    pub request_id: String,
    pub request_doc: String,
}

#[derive(Debug)]
pub struct Imported {
    pub header: String,
    pub close: Option<String>,
}

pub async fn source(actors: &RequestActors, session: &str) -> Source {
    let created_at = canonical_now();
    let session_doc = actors.session(session, &created_at).await;
    let request_id = format!("{session}:history");
    let request_doc = actors
        .create_request(RequestSeed::new(&request_id, session, &created_at).state("completed"))
        .await;
    Source {
        session: session.to_owned(),
        requester: actors.agent_did().to_owned(),
        session_doc,
        request_id,
        request_doc,
    }
}

/// The bound session-child fields the canonical writer adds for its actor.
fn bind(actors: &RequestActors, source: &Source, variables: &mut serde_json::Value) {
    let input = &mut variables["input"];
    input["session_doc_id"] = source.session_doc.clone().into();
    input["agent_did"] = actors.agent_did().into();
    input["creator_did"] = actors.agent_did().into();
    input["parent_linked"] = false.into();
}

async fn create_as_agent(
    actors: &RequestActors,
    collection: &str,
    mutation: &str,
    variables: serde_json::Value,
) -> String {
    actors
        .agent_access
        .transact_write("test.agent_transcript.row", |txn| {
            let variables = &variables;
            Box::pin(async move {
                let response = txn.execute_with_variables(mutation, variables).await?;
                Ok::<_, anyhow::Error>(gents::store::graphql::created_doc_id(
                    &response, collection,
                )?)
            })
        })
        .await
        .unwrap_or_else(|error| panic!("Agent writes its {collection}: {error:#}"))
}

/// One text message with its closing segment. Without `valid_ref` the header
/// names a closing record that does not exist.
pub async fn import(
    actors: &RequestActors,
    source: &Source,
    sequence: u32,
    role: MessageRole,
    text: &str,
    valid_ref: bool,
) -> Imported {
    let node_did = actors.node_did();
    let created_at = canonical_now();
    let close = if valid_ref {
        let segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: Some(source.requester.clone()),
            session_id: source.session.clone(),
            request_doc_id: source.request_doc.clone(),
            source: if role == MessageRole::Assistant {
                OutputSource::ProviderTurn {
                    scope: gents_protocol::rendered_request::CaptureScope {
                        kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                        seq: u64::from(sequence),
                    },
                    turn_index: 0,
                    attempt: 0,
                }
            } else {
                OutputSource::Authored {
                    key: format!("fixture:{sequence}"),
                }
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "fixture-generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: text.len().try_into().unwrap(),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![text.len() as u64],
            }),
            created_at: created_at.clone(),
        };
        let mut variables = output_segment_create_variables(&segment).unwrap();
        bind(actors, source, &mut variables);
        Some(
            create_as_agent(
                actors,
                "AgentOutputSegment",
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                variables,
            )
            .await,
        )
    } else {
        None
    };
    let message = TranscriptMessage {
        native_request_id: None,
        message_key: gents::session::sequence_message_key(
            node_did,
            &source.session,
            Some(&source.requester),
            sequence,
        ),
        session_id: source.session.clone(),
        node_did: node_did.into(),
        requester_did: Some(source.requester.clone()),
        request_doc_id: Some(source.request_doc.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role,
        native_id: (role == MessageRole::Assistant).then(|| format!("native-{sequence}")),
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close.clone().unwrap_or_else(|| "missing-close".into()),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at,
    };
    let mut variables = transcript_message_create_variables(&message).unwrap();
    bind(actors, source, &mut variables);
    Imported {
        header: create_as_agent(
            actors,
            "AgentMessage",
            CREATE_AGENT_MESSAGE_MUTATION,
            variables,
        )
        .await,
        close,
    }
}

/// A completed tool row as the accepted provider turn creates it.
pub async fn tool_call(
    actors: &RequestActors,
    source: &Source,
    message_sequence: u32,
    tool_call_id: &str,
) -> String {
    let mut variables = serde_json::json!({"input": {
        "tool_call_key": format!("{}:{tool_call_id}", source.request_doc),
        "request_id": source.request_id,
        "request_doc_id": source.request_doc,
        "session_id": source.session,
        "node_did": actors.node_did(),
        "requester_did": source.requester,
        "message_sequence": message_sequence,
        "tool_name": "read_file",
        "tool_call_id": tool_call_id,
        "status": "completed",
        "lifecycle_state": "completed",
        "started_at": canonical_now(),
        "completed_at": canonical_now(),
    }});
    bind(actors, source, &mut variables);
    create_as_agent(
        actors,
        "AgentToolCall",
        "mutation($input: AgentToolCallMutationInputArg!) { create_AgentToolCall(input: $input) { _docID } }",
        variables,
    )
    .await
}

/// A session-prefix compaction cursor as the compaction owner writes it.
pub async fn compaction(
    actors: &RequestActors,
    source: &Source,
    sequence: u32,
    summary: &str,
    messages_compacted: u32,
    compacted_through_sequence: u32,
) {
    let input = serde_json::json!({
        "compaction_key": gents::session::compaction_key(
            actors.node_did(), &source.session, Some(&source.requester), sequence,
        ),
        "session_id": source.session,
        "node_did": actors.node_did(),
        "requester_did": source.requester,
        "request_id": source.request_id,
        "request_doc_id": source.request_doc,
        "sequence": sequence,
        "summary": summary,
        "files_read": r#"{"version":1,"paths":[]}"#,
        "files_modified": r#"{"version":1,"paths":[]}"#,
        "messages_compacted": messages_compacted,
        "compacted_through_sequence": compacted_through_sequence,
        "original_tokens": 100,
        "compacted_tokens": 50,
        "created_at": canonical_now(),
    });
    actors
        .agent_access
        .transact_write("test.agent_transcript.compaction", |txn| {
            let input = &input;
            let session_doc = &source.session_doc;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: CompactionEntryMutationInputArg!) { create_CompactionEntry(input: $input) { _docID } }",
                        &serde_json::json!({"input": input}),
                    )
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&response, "CompactionEntry")?;
                txn.add_resource_relationship(
                    "CompactionEntry",
                    &doc,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("Agent records its compaction cursor");
}
