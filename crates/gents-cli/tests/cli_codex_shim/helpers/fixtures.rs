use super::*;
use crate::support::graphql::graphql_mutation_with_variables;

async fn exact_request_binding(
    graphql: &str,
    request_id: &str,
) -> Result<(String, String, Option<String>)> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID agent_did requester_did
            }} }}"#,
            escape_graphql_string(request_id),
        ),
    )
    .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("request binding query omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "request binding must resolve exactly once");
    let row = &rows[0];
    Ok((
        row.get("_docID")
            .and_then(Value::as_str)
            .context("request binding missing physical ID")?
            .to_string(),
        row.get("agent_did")
            .and_then(Value::as_str)
            .context("request binding missing principal")?
            .to_string(),
        row.get("requester_did")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    ))
}

/// The next free canonical sequence in a live session, chosen only after the
/// runtime has claimed `request_doc_id` and appended its prompt header.
/// Seeding earlier races that append: both headers would take the same
/// sequence, and canonical header lookup rejects sequence twins as conflicts.
async fn next_session_message_sequence(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    request_doc_id: &str,
) -> Result<u32> {
    let query = format!(
        r#"{{
            prompt: AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    request_doc_id: {{ _eq: "{request_doc_id}" }},
                    role: {{ _eq: "user" }}
                }},
                limit: 1
            ) {{ sequence }}
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }}
            ) {{ message_sequence }}
        }}"#,
        session_id = escape_graphql_string(session_id),
        agent_did = escape_graphql_string(agent_did),
        request_doc_id = escape_graphql_string(request_doc_id),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = graphql_query(graphql, &query).await?;
        let sequences = |alias: &str, field: &str| {
            response
                .pointer(&format!("/data/{alias}"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get(field).and_then(Value::as_u64))
                .collect::<Vec<_>>()
        };
        if !sequences("prompt", "sequence").is_empty() {
            let highest = sequences("AgentMessage", "sequence")
                .into_iter()
                .chain(sequences("AgentToolCall", "message_sequence"))
                .max()
                .unwrap_or(0);
            return u32::try_from(highest + 1).context("fixture session sequence exceeds u32");
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "runtime never appended the prompt header for request {request_doc_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn seed_canonical_tool_transcript(
    graphql: &str,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_doc_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    sequence: u32,
    arguments: &str,
    result: Option<&str>,
    created_at: &str,
) -> Result<()> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let access = ConfigAccess::Graphql(graphql.to_owned());
    let generation = format!("codex-fixture:{request_doc_id}:{sequence}");
    let argument_segment = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: u64::from(sequence),
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(arguments.len()).context("fixture arguments exceed u32")?,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: tool_call_id.into(),
                    call_id: Some(tool_call_id.into()),
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
        created_at: created_at.into(),
    };
    let argument_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&argument_segment)?,
    )
    .await?;
    let argument_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&argument_response, "AgentOutputSegment")?;
    let admission = TranscriptMessage {
        message_key: gents::session::sequence_message_key(
            agent_did,
            session_id,
            requester_did,
            sequence,
        ),
        session_id: session_id.into(),
        agent_did: agent_did.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: tool_call_doc_id.into(),
            id: tool_call_id.into(),
            call_id: Some(tool_call_id.into()),
            name: tool_name.into(),
            arguments: PayloadRef {
                close_doc_id: argument_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: created_at.into(),
    };
    graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&admission)?,
    )
    .await?;

    if let Some(result) = result {
        let result_segment = OutputSegment {
            agent_did: agent_did.into(),
            requester_did: requester_did.map(str::to_owned),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_call_doc_id.into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_call_doc_id.into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: u32::try_from(result.len()).context("fixture result exceeds u32")?,
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
            created_at: created_at.into(),
        };
        let result_response = graphql_mutation_with_variables(
            &access,
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&result_segment)?,
        )
        .await?;
        let result_doc_id = gents_protocol::graphql::extract_mutation_doc_id(
            &result_response,
            "AgentOutputSegment",
        )?;
        let delivery_sequence = 1000 + sequence;
        let delivery = TranscriptMessage {
            message_key: gents::session::sequence_message_key(
                agent_did,
                session_id,
                requester_did,
                delivery_sequence,
            ),
            session_id: session_id.into(),
            agent_did: agent_did.into(),
            requester_did: requester_did.map(str::to_owned),
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::ToolDelivery {
                tool_call_doc_id: tool_call_doc_id.into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: delivery_sequence,
            role: MessageRole::User,
            native_id: None,
            blocks: vec![MessageBlock::ToolResult {
                tool_call_doc_id: tool_call_doc_id.into(),
                id: tool_call_id.into(),
                call_id: Some(tool_call_id.into()),
                parts: vec![ToolResultPart::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: result_doc_id,
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
            }],
            created_at: created_at.into(),
        };
        graphql_mutation_with_variables(
            &access,
            CREATE_AGENT_MESSAGE_MUTATION,
            &transcript_message_create_variables(&delivery)?,
        )
        .await?;
    }
    // Fail at the seam rather than as a later projection timeout: the shim
    // reads this tool through the same canonical owner.
    let presentation = gents::tool_call_lifecycle::load_tool_call_presentation(
        &access,
        tool_call_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await
    .context("seeded tool call does not reconstruct through its canonical admission")?;
    anyhow::ensure!(
        serde_json::from_str::<Value>(&presentation.arguments)?
            == serde_json::from_str::<Value>(arguments)?
            && presentation.result.as_deref() == result,
        "seeded tool call reconstructs different canonical arguments or result"
    );
    Ok(())
}

pub(super) async fn initialize_config_and_thread(
    ws: &mut ShimWebSocket,
    _home_dir: &std::path::Path,
) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(101),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-live-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(ws, request_id(101)).await?;
    send_client_notification(ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(102),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let _: codex::ConfigReadResponse = read_typed_response(ws, request_id(102)).await?;
    Ok(())
}

pub(super) async fn start_thread(
    ws: &mut ShimWebSocket,
    home_dir: &std::path::Path,
) -> Result<String> {
    send_client_request(
        ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(103),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse = read_typed_response(ws, request_id(103)).await?;
    Ok(thread_start.thread.id)
}

pub(super) async fn send_turn(ws: &mut ShimWebSocket, thread_id: &str, prompt: &str) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(104),
            params: codex::TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(ws, request_id(104)).await?;
    Ok(())
}

pub(super) async fn seed_blank_materialized_completion(
    graphql: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
) -> Result<()> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let blank_assistant = "\n\n\n";
    let (request_doc_id, request_agent_did, requester_did) =
        exact_request_binding(graphql, request_id).await?;
    anyhow::ensure!(request_agent_did == agent_did);
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let _ = behavior_id;
    let generation = format!("codex-blank-{request_id}");
    let access = ConfigAccess::Graphql(graphql.to_owned());
    let segment = OutputSegment {
        agent_did: agent_did.to_owned(),
        requester_did: Some(agent_did.to_owned()),
        session_id: session_id.to_owned(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        ordinal: Some(0),
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        runs: vec![SegmentRun {
            stream: 0,
            bytes: blank_assistant.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: blank_assistant.to_owned(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![blank_assistant.len() as u64],
        }),
        created_at: now.clone(),
    };
    let segment_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&segment_response, "AgentOutputSegment")?;
    let header = TranscriptMessage {
        message_key: gents::session::sequence_message_key(
            agent_did,
            session_id,
            Some(agent_did),
            2,
        ),
        session_id: session_id.to_owned(),
        agent_did: agent_did.to_owned(),
        requester_did: Some(agent_did.to_owned()),
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence: 2,
        role: MessageRole::Assistant,
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
        created_at: now.clone(),
    };
    let header_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header)?,
    )
    .await?;
    let message_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&header_response, "AgentMessage")?;
    graphql_mutation_with_variables(
        &access,
        r#"mutation($doc: String!, $selection: JSON!, $now: String!) { update_AgentRequest(filter: { _docID: { _eq: $doc } }, input: { lifecycle_state: "completed", terminal_output: $selection, terminalized_at: $now, failure_reason: "" }) { _docID } }"#,
        &json!({"doc": request_doc_id, "selection": serde_json::to_value(TerminalOutput::Message { message_doc_id })?, "now": now}),
    ).await?;
    Ok(())
}

pub(super) async fn seed_running_background_tool(
    graphql: &str,
    request_id: &str,
    session_id: &str,
    tool_call_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let (request_doc_id, agent_did, requester_did) =
        exact_request_binding(graphql, request_id).await?;
    let sequence =
        next_session_message_sequence(graphql, &agent_did, session_id, &request_doc_id).await?;
    let requester_field = requester_did
        .as_deref()
        .map(|did| format!(r#"requester_did: "{}","#, escape_graphql_string(did)))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                {requester_field}
                message_sequence: {sequence},
                tool_name: "bash",
                tool_call_id: "codex-bg-interrupt",
                status: "called",
                lifecycle_state: "running",
                started_at: "{now}",
                await_mode: "background"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(tool_call_key),
        request_id = escape_graphql_string(request_id),
        request_doc_id = escape_graphql_string(&request_doc_id),
        session_id = escape_graphql_string(session_id),
        agent_did = escape_graphql_string(&agent_did),
        now = escape_graphql_string(&now),
    );
    let tool_response = graphql_query(graphql, &mutation).await?;
    let tool_call_doc_id = doc_id_from_create(&tool_response, "add_AgentToolCall")?;
    seed_canonical_tool_transcript(
        graphql,
        &request_doc_id,
        &agent_did,
        requester_did.as_deref(),
        session_id,
        &tool_call_doc_id,
        "codex-bg-interrupt",
        "bash",
        sequence,
        r#"{"command":"sleep 600"}"#,
        None,
        &now,
    )
    .await?;
    Ok(())
}

pub(super) async fn seed_background_completion_wake(
    graphql: &str,
    identity: &dyn gents::AgentIdentity,
    behavior_id: &str,
    session_id: &str,
) -> Result<String> {
    let agent_did = identity.did();
    let source_request_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();
    let source_created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let wake_created_at = (chrono::DateTime::parse_from_rfc3339(&source_created_at)?
        + chrono::Duration::seconds(1))
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let input = serde_json::from_value(json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": format!("background_completion:{session_id}"),
            "queued_after_request_id": null,
            "background_completion_wake_version": 1
        }
    }))?;
    let mut source = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &source_request_id,
        agent_did,
        agent_did,
        behavior_id,
        session_id,
        "completed source for background continuation",
        "interactive",
        &source_created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent_did),
    );
    gents::sign_agent_request_create(identity, &mut source).await?;
    let source_fields = source.graphql_input_fields().map_err(anyhow::Error::msg)?;
    // Publish the source and terminalize it in one DefraDB transaction so the
    // live watcher can never observe this fixture-only source as executable.
    let source_response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                source: create_AgentRequest(input: {{ {source_fields} }}) {{ _docID }}
                terminal: update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{}" }} }},
                    input: {{ lifecycle_state: "completed" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&source_request_id),
        ),
    )
    .await?;
    let source_doc_id = source_response
        .pointer("/data/source")
        .and_then(|value| {
            value.get("_docID").or_else(|| {
                value
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("_docID"))
            })
        })
        .and_then(serde_json::Value::as_str)
        .with_context(|| {
            format!("signed background source mutation returned no _docID: {source_response}")
        })?;

    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            agent_did,
            &source_request_id,
        );
    let mut wake = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &request_id,
        agent_did,
        agent_did,
        behavior_id,
        session_id,
        gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
        "scheduled",
        &wake_created_at,
        admission,
    );
    wake.input = input;
    wake.caused_by_parent_request_id = Some(source_request_id);
    wake.caused_by_parent_request_doc_id = Some(source_doc_id.to_string());
    gents::sign_agent_request_create(identity, &mut wake).await?;
    graphql_query(
        graphql,
        &wake.graphql_mutation().map_err(anyhow::Error::msg)?,
    )
    .await?;
    Ok(request_id)
}
