use super::*;

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
    let now = chrono::Utc::now().to_rfc3339();
    let message_key = format!("{session_id}:blank-terminal");
    let blank_assistant = "\n\n\n";
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                sequence: 2,
                role: "assistant",
                content: "{blank_assistant}",
                timestamp: "{now}"
            }}) {{ _docID }}
            upsert_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                add: {{
                    response_key: "{request_id}",
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    token_count: 0,
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    created_at: "{now}",
                    completed_at: "{now}"
                }},
                update: {{
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        message_key = escape_graphql_string(&message_key),
        session_id = escape_graphql_string(session_id),
        blank_assistant = escape_graphql_string(blank_assistant),
        now = escape_graphql_string(&now),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn seed_running_background_tool(
    graphql: &str,
    request_id: &str,
    session_id: &str,
    tool_call_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{request_id}",
                session_id: "{session_id}",
                message_sequence: 1,
                tool_name: "bash",
                tool_call_id: "codex-bg-interrupt",
                args: "{{\"command\":\"sleep 600\"}}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "{now}",
                await_mode: "background"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(tool_call_key),
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn seed_authorized_subagent_link(
    graphql: &str,
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
    let now = chrono::Utc::now().to_rfc3339();
    let args = serde_json::to_string(&json!({
        "name": "reviewer",
        "prompt": "Inspect the parent change"
    }))?;
    let result = serde_json::to_string(&json!({
        "child_request_id": child_request_id,
        "child_session_id": child_session_id
    }))?;
    let mutation = format!(
        r#"mutation {{
            create_AgentBehavior(input: {{
                behavior_id: "{child_behavior_id}",
                agent_did: "{agent_did}",
                display_name: "reviewer",
                system_prompt: "",
                backend_id: "{child_backend_id}",
                model_name: "{child_model_name}",
                tool_selection_id: "",
                inference_profile_id: "",
                compaction_strategy: "StripThenSummarize",
                compaction_threshold: 0.75,
                enabled: false,
                created_at: "{now}"
            }}) {{ _docID }}
            create_AgentSession(input: {{
                session_id: "{child_session_id}",
                agent_name: "reviewer",
                agent_did: "{agent_did}",
                behavior_id: "{child_behavior_id}",
                started: "{now}",
                status: "active"
            }}) {{ _docID }}
            create_AgentRequest(input: {{
                request_id: "{child_request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{child_behavior_id}",
                session_id: "{child_session_id}",
                content: "Inspect the parent change",
                metadata: "{{}}",
                status: "processing",
                lifecycle_state: "processing",
                execution_origin: "subagent",
                failure_reason: "",
                created_at: "{now}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 1,
                caused_by_parent_request_id: "{parent_request_id}",
                caused_by_parent_tool_call_id: "{tool_call_id}"
            }}) {{ _docID }}
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{tool_call_id}",
                args: "{args}",
                result: "{result}",
                status: "completed",
                lifecycle_state: "completed",
                child_request_id: "{child_request_id}",
                spawn_target_did: "{agent_did}",
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        child_session_id = escape_graphql_string(child_session_id),
        agent_did = escape_graphql_string(agent_did),
        child_behavior_id = escape_graphql_string(child_behavior_id),
        child_backend_id = escape_graphql_string(child_backend_id),
        child_model_name = escape_graphql_string(child_model_name),
        now = escape_graphql_string(&now),
        child_request_id = escape_graphql_string(child_request_id),
        parent_request_id = escape_graphql_string(parent_request_id),
        tool_call_id = escape_graphql_string(tool_call_id),
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_session_id = escape_graphql_string(parent_session_id),
        args = escape_graphql_string(&args),
        result = escape_graphql_string(&result),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn delete_agent_behavior(graphql: &str, behavior_id: &str) -> Result<()> {
    let behavior_id = escape_graphql_string(behavior_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                delete_AgentBehavior(filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await?;
    anyhow::ensure!(
        response
            .pointer("/data/delete_AgentBehavior")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()),
        "expected child AgentBehavior to be deleted: {response}"
    );
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
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                message_sequence: 2,
                tool_name: "spawn_subagent",
                tool_call_id: "unresolved-spawn",
                args: "{args}",
                result: "{result}",
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
        parent_session_id = escape_graphql_string(parent_session_id),
        agent_did = escape_graphql_string(agent_did),
        args = escape_graphql_string(&args),
        result = escape_graphql_string(&result),
        missing_child_request_id = escape_graphql_string(&missing_child_request_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(completed_at_ms)
}

pub(super) async fn seed_child_streaming_response(
    graphql: &str,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
    reasoning: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let created_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{request_id}",
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                content: "{content}",
                reasoning: "{reasoning}",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 1,
                reasoning_progress_seq: 1,
                created_at: "{now}",
                completed_at: ""
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
        session_id = escape_graphql_string(session_id),
        content = escape_graphql_string(content),
        reasoning = escape_graphql_string(reasoning),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(created_at_ms)
}

pub(super) async fn update_streaming_response_reasoning(
    graphql: &str,
    request_id: &str,
    reasoning: &str,
    reasoning_progress_seq: i64,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    reasoning: "{reasoning}",
                    reasoning_progress_seq: {reasoning_progress_seq}
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        reasoning = escape_graphql_string(reasoning),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn materialize_child_response_before_terminal(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    reasoning: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let materialized_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let message_key = format!("{session_id}:2");
    let content = r#"{"role":"assistant","id":null,"content":[{"text":"durable child answer"}]}"#;
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                request_id: "{request_id}",
                sequence: 2,
                role: "assistant",
                content: "{content}",
                reasoning: "{reasoning}",
                timestamp: "{now}"
            }}) {{ _docID }}
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    content: "",
                    reasoning: "",
                    progress_seq: 2,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        message_key = escape_graphql_string(&message_key),
        session_id = escape_graphql_string(session_id),
        agent_did = escape_graphql_string(agent_did),
        request_id = escape_graphql_string(request_id),
        content = escape_graphql_string(content),
        reasoning = escape_graphql_string(reasoning),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(materialized_at_ms)
}

pub(super) async fn finalize_child_response_after_materialization(
    graphql: &str,
    request_id: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "complete",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(completed_at_ms)
}

pub(super) async fn update_request_lifecycle(
    graphql: &str,
    request_id: &str,
    lifecycle_state: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "{lifecycle_state}",
                    lifecycle_state: "{lifecycle_state}",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        lifecycle_state = escape_graphql_string(lifecycle_state),
    );
    graphql_query(graphql, &mutation).await?;
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
    let metadata = serde_json::to_string(&json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": format!("background_completion:{session_id}"),
            "queued_after_request_id": null
        },
        "background_completion_wake_version": 1
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
                    input: {{ status: "completed", lifecycle_state: "completed" }}
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
    wake.metadata = Some(metadata);
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
