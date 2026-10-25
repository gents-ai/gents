use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::defra_node::QueryRequest;
use gents::graphql::{ensure_no_errors, single_mutation_document};
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};
use serde_json::json;

pub(crate) async fn insert_output_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
    let response = node
        .execute_request(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(segment).unwrap()),
        )
        .await;
    assert!(
        !response.has_errors(),
        "segment seed: {:?}",
        response.errors
    );
    single_mutation_document(&response, "create_AgentOutputSegment")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_string()
}

pub(crate) async fn insert_transcript_header(node: &EmbeddedNode, header: &TranscriptMessage) {
    let response = node
        .execute_request(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(header).unwrap()),
        )
        .await;
    assert!(
        !response.has_errors(),
        "message seed: {:?}",
        response.errors
    );
}

pub(crate) async fn next_request_sequence(node: &EmbeddedNode, request_doc_id: &str) -> u32 {
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{request_doc_id}"}}}}, limit: 2) {{session_id}} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "request scope lookup: {:?}",
        response.errors
    );
    let requests = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(requests.len(), 1, "fixture request scope must be exact");
    let session_id = gents::graphql::escape_graphql_string(
        requests[0]["session_id"].as_str().expect("fixture session"),
    );
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{session_id}"}}}}, order: [{{sequence: DESC}}], limit: 1) {{sequence}} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "sequence lookup: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("sequence"))
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |value| u32::try_from(value).unwrap())
        .checked_add(1)
        .expect("fixture transcript sequence overflow")
}

/// Seed one physical tool row through a matching canonical ToolCall admission
/// and, for terminal lifecycle states, one exact ToolDelivery observation.
#[allow(clippy::too_many_arguments)]

pub(crate) async fn seed_canonical_tool_call(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    tool_call_id: &str,
    tool_name: &str,
    lifecycle_state: &str,
    arguments: &str,
    result: Option<&str>,
    spawned_by_tool_call_doc_id: Option<&str>,
    message_sequence: Option<u32>,
    created_at: Option<&str>,
) -> String {
    use gents_protocol::output::ToolResultPart;

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let source_sequence = next_request_sequence(node, request_doc_id).await;
    let sequence = if let Some(sequence) = message_sequence {
        sequence
    } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        let parent = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{
                    request_doc_id node_did requester_did session_id tool_name message_sequence
                }} }}"#,
                gents::graphql::escape_graphql_string(parent_doc_id)
            ))
            .await;
        assert!(!parent.has_errors(), "spawn parent: {:?}", parent.errors);
        let rows = parent.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "spawn fixture requires one physical parent");
        let parent = &rows[0];
        assert_eq!(parent["request_doc_id"].as_str(), Some(request_doc_id));
        assert_eq!(parent["node_did"].as_str(), Some(node_did));
        assert_eq!(parent["requester_did"].as_str(), requester_did.as_deref());
        assert_eq!(parent["session_id"].as_str(), Some(session_id));
        assert_eq!(parent["tool_name"].as_str(), Some("spawn_process"));
        u32::try_from(parent["message_sequence"].as_u64().unwrap()).unwrap()
    } else {
        source_sequence
    };
    let created_at = created_at
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let requester = requester_did
        .as_deref()
        .map(|did| format!("\"{}\"", gents::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let spawned_by = spawned_by_tool_call_doc_id
        .map(|id| format!("\"{}\"", gents::graphql::escape_graphql_string(id)))
        .unwrap_or_else(|| "null".into());
    let tool_mutation = format!(
        r#"mutation {{ create_AgentToolCall(input: {{
        tool_call_key: "{}:{}", request_id: "{}", request_doc_id: "{}",
        node_did: "{}", requester_did: {requester}, session_id: "{}",
        tool_call_id: "{}", tool_name: "{}", message_sequence: {sequence},
        lifecycle_state: "{}", spawned_by_tool_call_doc_id: {spawned_by}, started_at: "{}"
    }}) {{_docID}} }}"#,
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(&request.request_id),
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(session_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(tool_name),
        gents::graphql::escape_graphql_string(lifecycle_state),
        gents::graphql::escape_graphql_string(&created_at),
    );
    let is_spawned_child = spawned_by_tool_call_doc_id.is_some();
    if is_spawned_child {
        assert!(
            result.is_none(),
            "spawned fixture results must be delivered by their lifecycle owner"
        );
    }
    let generation = format!("fixture:{request_doc_id}");
    let admitted = OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: format!("inference.{}", source_sequence.saturating_add(1))
                .parse()
                .unwrap(),
            turn_index: source_sequence,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: tool_call_id.into(),
                    call_id: None,
                    name: tool_name.into(),
                },
            }),
        }],
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    let admitted_variables = output_segment_create_variables(&admitted).unwrap();
    let admitted_generation = generation.clone();
    let requester_did_for_header = requester_did.clone();
    let node_did_for_header = node_did.to_owned();
    let session_id_for_header = session_id.to_owned();
    let request_doc_id_for_header = request_doc_id.to_owned();
    let tool_call_id_for_header = tool_call_id.to_owned();
    let tool_name_for_header = tool_name.to_owned();
    let created_at_for_header = created_at.clone();
    let message_key = format!("accepted:{request_doc_id}:{sequence}");
    let tool_doc_id = gents::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "grok.seed_canonical_tool_call",
        move |txn| {
            let tool_mutation = tool_mutation.clone();
            let admitted_variables = admitted_variables.clone();
            let admitted_generation = admitted_generation.clone();
            let requester_did_for_header = requester_did_for_header.clone();
            let node_did_for_header = node_did_for_header.clone();
            let session_id_for_header = session_id_for_header.clone();
            let request_doc_id_for_header = request_doc_id_for_header.clone();
            let tool_call_id_for_header = tool_call_id_for_header.clone();
            let tool_name_for_header = tool_name_for_header.clone();
            let created_at_for_header = created_at_for_header.clone();
            let message_key = message_key.clone();
            Box::pin(async move {
                let tool_response = txn.execute(&tool_mutation).await?;
                let tool_doc_id = gents_protocol::graphql::extract_mutation_doc_id(
                    &tool_response,
                    "AgentToolCall",
                )?;
                if is_spawned_child {
                    return Ok(tool_doc_id);
                }
                let admitted_response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &admitted_variables,
                    )
                    .await?;
                let admitted_doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &admitted_response,
                    "AgentOutputSegment",
                )?;
                let header = TranscriptMessage {
                    message_key,
                    session_id: session_id_for_header,
                    node_did: node_did_for_header,
                    requester_did: requester_did_for_header,
                    request_doc_id: Some(request_doc_id_for_header),
                    publication: MessagePublication::RequestExecution {
                        execution_generation: admitted_generation,
                    },
                    outcome: OutputOutcome::Complete,
                    sequence,
                    role: MessageRole::Assistant,
                    native_id: None,
                    blocks: vec![MessageBlock::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                        id: tool_call_id_for_header,
                        call_id: None,
                        name: tool_name_for_header,
                        arguments: PayloadRef {
                            close_doc_id: admitted_doc,
                            stream: 0,
                        },
                        signature: None,
                        additional_params: None,
                    }],
                    created_at: created_at_for_header,
                };
                let header_variables = transcript_message_create_variables(&header)?;
                txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &header_variables)
                    .await?;
                Ok(tool_doc_id)
            })
        },
    )
    .await
    .expect("atomically seed canonical tool admission");
    if spawned_by_tool_call_doc_id.is_some() {
        return tool_doc_id;
    }
    if let Some(result) = result {
        let delivery_sequence = next_request_sequence(node, request_doc_id).await;
        let delivered = OutputSegment {
            node_did: node_did.into(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: result.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: result.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![result.len() as u64],
            }),
            created_at: created_at.clone(),
        };
        let delivered_doc = insert_output_segment(node, &delivered).await;
        insert_transcript_header(
            node,
            &TranscriptMessage {
                message_key: format!("delivery:{request_doc_id}:{tool_call_id}"),
                session_id: session_id.into(),
                node_did: node_did.into(),
                requester_did,
                request_doc_id: Some(request_doc_id.into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_doc_id.clone(),
                },
                outcome: OutputOutcome::Complete,
                sequence: delivery_sequence,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_doc_id.clone(),
                    id: tool_call_id.into(),
                    call_id: None,
                    parts: vec![ToolResultPart::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: delivered_doc,
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                }],
                created_at,
            },
        )
        .await;
    }
    tool_doc_id
}

/// Publish the background receipt an accepted background row carries before
/// its terminal, in the shape `publish_background_receipt_in_txn` writes.

pub(crate) async fn seed_background_receipt(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    tool_call_id: &str,
) {
    use gents_protocol::output::ToolResultPart;

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let text = "started";
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: request.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored { key: key.clone() },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc_id.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
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
    let close_doc_id = insert_output_segment(node, &segment).await;
    let sequence = next_request_sequence(node, request_doc_id).await;
    insert_transcript_header(
        node,
        &TranscriptMessage {
            message_key: key,
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: request.requester_did.clone(),
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::ToolDelivery {
                tool_call_doc_id: tool_doc_id.into(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::User,
            native_id: None,
            blocks: vec![MessageBlock::ToolResult {
                tool_call_doc_id: tool_doc_id.into(),
                id: tool_call_id.into(),
                call_id: None,
                parts: vec![ToolResultPart::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id,
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
            }],
            created_at,
        },
    )
    .await;
}

/// Complete an already-admitted ordinary tool through its canonical output
/// and ToolDelivery owners, then publish the terminal lifecycle marker.

pub(crate) async fn seed_job_row(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session: &str,
    call: &str,
    handle: &str,
    lifecycle: &str,
    state: gents_protocol::job::JobState,
) -> gents_protocol::job::JobView {
    use gents_protocol::job::{JobBudget, JobCeiling, JobOrigin, JobPin, JobProgress};
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: handle.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let progress = JobProgress {
        title: format!("Job {call}"),
        state,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let ended = if lifecycle == "running" {
        ""
    } else {
        r#"completed_at: "2026-06-01T10:05:00Z","#
    };
    let request = seed_signed_request(
        node,
        node_did,
        session,
        &format!("request-{call}"),
        "2026-06-01T10:00:00Z",
    )
    .await;
    let doc_id = seed_canonical_tool_call(
        node, &request, call, "workflow", lifecycle, "{}", None, None, None, None,
    )
    .await;
    pin.chain_root_doc_id = doc_id.clone();
    let escaped_doc_id = gents::graphql::escape_graphql_string(&doc_id);
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{
            await_mode: "background", status: "{lifecycle}", deadline_at: "2126-06-01T10:00:00Z", {ended}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#
    );
    let variables = json!({ "job_pin": pin, "job_progress": progress });
    ConfigAccess::transact_local_write(node, None, "test.grok.workflow.job", |txn| {
        let (mutation, variables) = (&mutation, &variables);
        Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
    })
    .await
    .unwrap();
    if lifecycle == "running" {
        seed_background_receipt(node, &request, &doc_id, call).await;
    }
    gents::plugin_job::view::load_job_view(
        &ConfigAccess::local(node.clone()),
        None,
        gents::plugin_job::view::JobKey::Address {
            session_id: session.into(),
            tool_call_id: call.into(),
        },
    )
    .await
    .unwrap()
}

/// A pending `local-self` request authored through the production request
/// builder and signed by `node_did`'s registered identity, so readers that
/// verify the owning request's admission accept the rows seeded under it.

async fn seed_signed_request(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    created_at: &str,
) -> gents_protocol::row::AgentRequestRow {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let create = gents::build_signed_request(
        gents::RequestSpec::new(
            RequestPurpose::Normal,
            gents::RequestIdentity {
                request_id: request_id.into(),
                node_did: node_did.into(),
                requester_did: None,
                agent_id: "test".into(),
                session_id: session_id.into(),
                content: "test".into(),
                execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::local_self(node_did),
        ),
        gents::RequestSigner::RegisteredTarget,
    )
    .await
    .unwrap();
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    ensure_no_errors(&created, "seed signed request").unwrap();
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents::graphql::escape_graphql_string(request_id),
            gents::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    ensure_no_errors(&row, "seeded signed request row").unwrap();
    serde_json::from_value(row.data.unwrap()["AgentRequest"][0].clone()).unwrap()
}
