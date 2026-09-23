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
                message_sequence: 1,
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
        1,
        r#"{"command":"sleep 600"}"#,
        None,
        &now,
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn seed_authorized_subagent_link(
    graphql: &str,
    identity: &dyn gents::AgentIdentity,
    agent_did: &str,
    child_behavior_id: &str,
    parent_request_id: &str,
    parent_session_id: &str,
    child_request_id: &str,
    child_session_id: &str,
    tool_call_id: &str,
    tool_call_key: &str,
    child_backend_id: &str,
    child_model_name: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (parent_request_doc_id, parent_agent_did, parent_requester_did) =
        exact_request_binding(graphql, parent_request_id).await?;
    anyhow::ensure!(parent_agent_did == agent_did, "parent principal changed");
    anyhow::ensure!(
        parent_requester_did.as_deref() == Some(agent_did),
        "parent requester changed"
    );
    let args = serde_json::to_string(&json!({
        "name": "reviewer",
        "prompt": "Inspect the parent change"
    }))?;
    let result = serde_json::to_string(&json!({
        "child_request_id": child_request_id,
        "child_session_id": child_session_id
    }))?;
    let config_mutation = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{child_backend_id}",
                agent_did: "{agent_did}",
                name: "Child projection backend",
                provider_kind: "OpenAiCompatible",
                openai_wire_api: "chat_completions",
                endpoint: "http://127.0.0.1:1/v1",
                auth: {{kind: "unauthenticated"}},
                enabled: true
            }}) {{ _docID }}
            create_InferenceProfile(input: {{
                profile_id: "{child_profile_id}",
                agent_did: "{agent_did}",
                backend_id: "{child_backend_id}",
                model_name: "{child_model_name}"
            }}) {{ _docID }}
            create_AgentBehavior(input: {{
                behavior_id: "{child_behavior_id}",
                agent_did: "{agent_did}",
                display_name: "reviewer",
                inference_profile_id: "{child_profile_id}",
                enabled: true,
                created_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        child_profile_id = escape_graphql_string(&format!("{child_behavior_id}:profile")),
        agent_did = escape_graphql_string(agent_did),
        child_behavior_id = escape_graphql_string(child_behavior_id),
        child_backend_id = escape_graphql_string(child_backend_id),
        child_model_name = escape_graphql_string(child_model_name),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &config_mutation).await?;

    let tool_mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                request_doc_id: "{parent_request_doc_id}",
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{tool_call_id}",
                status: "completed",
                lifecycle_state: "completed",
                child_request_id: "{child_request_id}",
                spawn_target_did: "{agent_did}",
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        now = escape_graphql_string(&now),
        child_request_id = escape_graphql_string(child_request_id),
        parent_request_id = escape_graphql_string(parent_request_id),
        parent_request_doc_id = escape_graphql_string(&parent_request_doc_id),
        tool_call_id = escape_graphql_string(tool_call_id),
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_session_id = escape_graphql_string(parent_session_id),
    );
    let tool_response = graphql_query(graphql, &tool_mutation).await?;
    let tool_call_doc_id = first_graphql_row(&tool_response, "add_AgentToolCall")?
        .get("_docID")
        .and_then(Value::as_str)
        .context("spawn tool call missing physical ID")?;
    seed_canonical_tool_transcript(
        graphql,
        &parent_request_doc_id,
        agent_did,
        Some(agent_did),
        parent_session_id,
        tool_call_doc_id,
        tool_call_id,
        "spawn_subagent",
        1,
        &args,
        Some(&result),
        &now,
    )
    .await?;

    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
            agent_did,
            parent_request_id,
        );
    let child = gents::build_signed_request(
        gents::RequestSpec {
            subagent: Some(gents::ParentLink {
                depth: 1,
                parent_request_id: parent_request_id.to_string(),
                parent_request_doc_id: parent_request_doc_id.clone(),
                parent_tool_call_id: Some(tool_call_id.to_string()),
                parent_tool_call_doc_id: Some(tool_call_doc_id.to_string()),
            }),
            ..gents::RequestSpec::new(
                gents::RequestIdentity {
                    requester_did: None,
                    request_id: child_request_id.to_string(),
                    agent_did: agent_did.to_string(),
                    behavior_id: child_behavior_id.to_string(),
                    session_id: child_session_id.to_string(),
                    content: "Inspect the parent change".to_string(),
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: now.clone(),
                },
                admission,
            )
        },
        gents::RequestSigner::Identity(identity),
    )
    .await?;
    let child_fields = child.graphql_input_fields().map_err(anyhow::Error::msg)?;
    let child_response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                child: create_AgentRequest(input: {{ {child_fields} }}) {{ _docID }}
                active: update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{}" }} }},
                    input: {{ lifecycle_state: "processing" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(child_request_id),
        ),
    )
    .await?;
    let child_request_doc_id = first_graphql_row(&child_response, "child")?
        .get("_docID")
        .and_then(Value::as_str)
        .context("child request missing physical ID")?;
    let session = gents_protocol::session::AgentSession {
        session_id: child_session_id.to_string(),
        agent_did: agent_did.to_string(),
        requester_did: Some(agent_did.to_string()),
        behavior_id: child_behavior_id.to_string(),
        created_at: now.clone(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: Some(gents_protocol::session::SessionProvenance {
            parent_request_doc_id: Some(parent_request_doc_id),
            ..Default::default()
        }),
        observation: Some(gents_protocol::session::SessionObservation {
            last_activity_at: now,
            preview: Some("Inspect the parent change".to_string()),
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: child_request_doc_id.to_string(),
                request_id: child_request_id.to_string(),
                lifecycle_state:
                    gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
            }),
        }),
    };
    let session_input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session)?)?;
    graphql_query(
        graphql,
        &format!("mutation {{ create_AgentSession(input: {session_input}) {{ _docID }} }}"),
    )
    .await?;
    Ok(())
}

pub(super) async fn seed_unresolved_completed_subagent_tool(
    graphql: &str,
    agent_did: &str,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_key: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let (parent_request_doc_id, _, requester_did) =
        exact_request_binding(graphql, parent_request_id).await?;
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let completed_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let missing_child_request_id = Uuid::new_v4().to_string();
    let args = serde_json::to_string(&json!({
        "name": "replication-lagged",
        "prompt": "This child edge is intentionally unavailable"
    }))?;
    let result = serde_json::to_string(&json!({
        "child_request_id": missing_child_request_id
    }))?;
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                request_doc_id: "{parent_request_doc_id}",
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                message_sequence: 2,
                tool_name: "spawn_subagent",
                tool_call_id: "unresolved-spawn",
                status: "completed",
                lifecycle_state: "completed",
                child_request_id: "{missing_child_request_id}",
                spawn_target_did: "{agent_did}",
                selected_service_id: "runtime-subagents",
                selected_tool_name: "spawn",
                latency_ms: 23,
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_request_id = escape_graphql_string(parent_request_id),
        parent_request_doc_id = escape_graphql_string(&parent_request_doc_id),
        parent_session_id = escape_graphql_string(parent_session_id),
        agent_did = escape_graphql_string(agent_did),
        missing_child_request_id = escape_graphql_string(&missing_child_request_id),
        now = escape_graphql_string(&now),
    );
    let tool_response = graphql_query(graphql, &mutation).await?;
    let tool_call_doc_id = doc_id_from_create(&tool_response, "add_AgentToolCall")?;
    seed_canonical_tool_transcript(
        graphql,
        &parent_request_doc_id,
        agent_did,
        Some(agent_did),
        parent_session_id,
        &tool_call_doc_id,
        "unresolved-spawn",
        "spawn_subagent",
        2,
        &args,
        Some(&result),
        &now,
    )
    .await?;
    Ok(completed_at_ms)
}

pub(super) async fn seed_child_streaming_response(
    graphql: &str,
    agent_did: &str,
    _behavior_id: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
    reasoning: &str,
) -> Result<i64> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let now = chrono::Utc::now().to_rfc3339();
    let created_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let (request_doc_id, _, requester_did) = exact_request_binding(graphql, request_id).await?;
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let generation = format!("codex-child-{request_id}");
    let lease_expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    graphql_mutation_with_variables(
        &ConfigAccess::Graphql(graphql.to_owned()),
        r#"mutation($doc: String!, $generation: String!, $expires: String!) {
                update_AgentRequest(filter: { _docID: { _eq: $doc } }, input: {
                    lifecycle_state: "processing", execution_generation: $generation,
                    execution_lease_secs: 300, execution_lease_expires_at: $expires
                }) { _docID }
            }"#,
        &json!({"doc": request_doc_id, "generation": generation, "expires": lease_expires}),
    )
    .await?;
    let text_bytes = u32::try_from(content.len()).context("live child text exceeds u32")?;
    let reasoning_bytes = u32::try_from(reasoning.len()).context("live reasoning exceeds u32")?;
    let segment = OutputSegment {
        agent_did: agent_did.to_owned(),
        requester_did: Some(agent_did.to_owned()),
        session_id: session_id.to_owned(),
        request_doc_id,
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
            execution_generation: generation,
        },
        runs: vec![
            SegmentRun {
                stream: 0,
                bytes: text_bytes,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            },
            SegmentRun {
                stream: 1,
                bytes: reasoning_bytes,
                declaration: Some(StreamDeclaration {
                    block_index: 1,
                    part_index: 0,
                    payload: StreamPayload::Reasoning,
                }),
            },
        ],
        payload: format!("{content}{reasoning}"),
        close: None,
        created_at: now,
    };
    graphql_mutation_with_variables(
        &ConfigAccess::Graphql(graphql.to_owned()),
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    Ok(created_at_ms)
}

async fn canonical_child_output(
    graphql: &str,
    request_id: &str,
) -> Result<(
    gents_protocol::row::AgentRequestRow,
    gents::session::CanonicalRequestOutput,
)> {
    let response = graphql_query(graphql, &format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id agent_did behavior_id content lifecycle_state failure_reason terminal_output terminalized_at execution_generation execution_lease_secs execution_lease_expires_at retry_root_request requester_did session_id input }} }}"#, escape_graphql_string(request_id))).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("child request rows missing")?;
    anyhow::ensure!(rows.len() == 1, "child request must resolve exactly once");
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
    let output = gents::session::observe_request_output(
        &gents::config_client::ConfigAccess::Graphql(graphql.to_owned()),
        &row,
    )
    .await?;
    Ok((row, output))
}

pub(super) async fn update_streaming_response_reasoning(
    graphql: &str,
    request_id: &str,
    reasoning: &str,
    reasoning_progress_seq: i64,
) -> Result<()> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter, SegmentRun};
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let (request, output) = canonical_child_output(graphql, request_id).await?;
    let current = match &output {
        gents::session::CanonicalRequestOutput::Live(value)
        | gents::session::CanonicalRequestOutput::Settling(value)
        | gents::session::CanonicalRequestOutput::Published {
            presentation: value,
            ..
        }
        | gents::session::CanonicalRequestOutput::TerminalMessage {
            presentation: value,
            ..
        } => value,
        other => anyhow::bail!("child output has no current canonical presentation: {other:?}"),
    };
    let delta = reasoning
        .strip_prefix(current.reasoning_markdown.as_deref().unwrap_or(""))
        .context("reasoning update must append to canonical prefix")?;
    let segment = OutputSegment {
        agent_did: request.agent_did.clone().context("child agent DID")?,
        requester_did: request.requester_did.clone(),
        session_id: request.session_id.clone().context("child session")?,
        request_doc_id: request.doc_id.clone().context("child physical request")?,
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        ordinal: Some(u32::try_from(reasoning_progress_seq - 1).context("reasoning ordinal")?),
        writer: OutputWriter::RequestExecution {
            execution_generation: request
                .execution_generation
                .clone()
                .context("child generation")?,
        },
        runs: vec![SegmentRun {
            stream: 1,
            bytes: u32::try_from(delta.len()).context("reasoning delta exceeds u32")?,
            declaration: None,
        }],
        payload: delta.to_owned(),
        close: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    graphql_mutation_with_variables(
        &ConfigAccess::Graphql(graphql.to_owned()),
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    Ok(())
}

pub(super) async fn materialize_child_response_before_terminal(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    reasoning: &str,
) -> Result<i64> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart,
        SourceClose, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let now = chrono::Utc::now().to_rfc3339();
    let materialized_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let (request, output) = canonical_child_output(graphql, request_id).await?;
    anyhow::ensure!(request.agent_did.as_deref() == Some(agent_did));
    anyhow::ensure!(request.session_id.as_deref() == Some(session_id));
    let presentation = match output {
        gents::session::CanonicalRequestOutput::Live(value)
        | gents::session::CanonicalRequestOutput::Settling(value) => value,
        other => anyhow::bail!("child must be live before publication: {other:?}"),
    };
    anyhow::ensure!(presentation.reasoning_markdown.as_deref() == Some(reasoning));
    let generation = request.execution_generation.context("child generation")?;
    let request_doc_id = request.doc_id.context("child physical request")?;
    let source = OutputSource::ProviderTurn {
        scope: CaptureScope {
            kind: CaptureScopeKind::Inference,
            seq: 1,
        },
        turn_index: 0,
        attempt: 0,
    };
    let close_segment = OutputSegment {
        agent_did: agent_did.to_owned(),
        requester_did: Some(agent_did.to_owned()),
        session_id: session_id.to_owned(),
        request_doc_id: request_doc_id.clone(),
        source: source.clone(),
        ordinal: Some(2),
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        runs: Vec::new(),
        payload: String::new(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 3,
            stream_bytes: vec![
                presentation.body_markdown.len() as u64,
                presentation
                    .reasoning_markdown
                    .as_deref()
                    .map_or(0, str::len) as u64,
            ],
        }),
        created_at: now.clone(),
    };
    let access = ConfigAccess::Graphql(graphql.to_owned());
    let close_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&close_segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&close_response, "AgentOutputSegment")?;
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
        request_doc_id: Some(request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence: 2,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![
            MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: close_doc_id.clone(),
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            },
            MessageBlock::Reasoning {
                id: None,
                parts: vec![ReasoningPart::Text {
                    text: PayloadRef {
                        close_doc_id,
                        stream: 1,
                    },
                    signature: None,
                }],
            },
        ],
        created_at: now,
    };
    graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header)?,
    )
    .await?;
    Ok(materialized_at_ms)
}

pub(super) async fn finalize_child_response_after_materialization(
    graphql: &str,
    request_id: &str,
) -> Result<i64> {
    use gents::config_client::ConfigAccess;
    use gents_protocol::output::TerminalOutput;
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let (request, output) = canonical_child_output(graphql, request_id).await?;
    match output {
        gents::session::CanonicalRequestOutput::Published { .. } => {}
        other => anyhow::bail!("child must be published before terminal selection: {other:?}"),
    }
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("child physical request")?;
    let message_key = gents::session::sequence_message_key(
        request.agent_did.as_deref().context("child agent DID")?,
        request.session_id.as_deref().context("child session")?,
        request.requester_did.as_deref(),
        2,
    );
    let response = graphql_query(graphql, &format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
        escape_graphql_string(&message_key), escape_graphql_string(request_doc_id),
        escape_graphql_string(request.agent_did.as_deref().unwrap()),
        escape_graphql_string(request.requester_did.as_deref().context("child requester DID")?),
    )).await?;
    let rows = response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .context("published exact header rows")?;
    anyhow::ensure!(rows.len() == 1, "published exact header must resolve once");
    let message_doc_id = rows[0]
        .get("_docID")
        .and_then(Value::as_str)
        .context("published header physical ID")?
        .to_owned();
    graphql_mutation_with_variables(
        &ConfigAccess::Graphql(graphql.to_owned()),
        r#"mutation($doc: String!, $selection: JSON!, $now: String!) { update_AgentRequest(filter: { _docID: { _eq: $doc } }, input: { lifecycle_state: "completed", terminal_output: $selection, terminalized_at: $now, failure_reason: "" }) { _docID } }"#,
        &json!({"doc": request.doc_id, "selection": serde_json::to_value(TerminalOutput::Message { message_doc_id })?, "now": now}),
    ).await?;
    Ok(completed_at_ms)
}

pub(super) async fn update_request_lifecycle(
    graphql: &str,
    request_id: &str,
    lifecycle_state: &str,
) -> Result<()> {
    use gents::config_client::ConfigAccess;
    use gents_protocol::output::TerminalOutput;
    let (request_doc_id, _, _) = exact_request_binding(graphql, request_id).await?;
    graphql_mutation_with_variables(
        &ConfigAccess::Graphql(graphql.to_owned()),
        r#"mutation($doc: String!, $state: String!, $selection: JSON!, $now: String!) {
                update_AgentRequest(filter: { _docID: { _eq: $doc } }, input: {
                    lifecycle_state: $state, failure_reason: "",
                    terminal_output: $selection, terminalized_at: $now
                }) { _docID }
            }"#,
        &json!({
            "doc": request_doc_id,
            "state": lifecycle_state,
            "selection": serde_json::to_value(TerminalOutput::NoMessage)?,
            "now": chrono::Utc::now().to_rfc3339(),
        }),
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
