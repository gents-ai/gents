use super::*;

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
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let message_key = format!("{session_id}:blank-terminal");
    let blank_assistant = "\n\n\n";
    let (request_doc_id, request_agent_did, requester_did) =
        exact_request_binding(graphql, request_id).await?;
    anyhow::ensure!(request_agent_did == agent_did);
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let request_doc_id = request_doc_id.as_str();
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
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
                    request_doc_id: "{request_doc_id}",
                    agent_did: "{agent_did}",
                    requester_did: "{agent_did}",
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
                filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }},
                input: {{
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
        request_doc_id = escape_graphql_string(request_doc_id),
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
        request_doc_id = escape_graphql_string(&request_doc_id),
        session_id = escape_graphql_string(session_id),
        agent_did = escape_graphql_string(&agent_did),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
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
        agent_did = escape_graphql_string(agent_did),
        now = escape_graphql_string(&now),
        child_request_id = escape_graphql_string(child_request_id),
        parent_request_id = escape_graphql_string(parent_request_id),
        parent_request_doc_id = escape_graphql_string(&parent_request_doc_id),
        tool_call_id = escape_graphql_string(tool_call_id),
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_session_id = escape_graphql_string(parent_session_id),
        args = escape_graphql_string(&args),
        result = escape_graphql_string(&result),
    );
    let tool_response = graphql_query(graphql, &tool_mutation).await?;
    let tool_call_doc_id = first_graphql_row(&tool_response, "add_AgentToolCall")?
        .get("_docID")
        .and_then(Value::as_str)
        .context("spawn tool call missing physical ID")?;

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
        parent_request_doc_id = escape_graphql_string(&parent_request_doc_id),
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
    let (request_doc_id, _, requester_did) = exact_request_binding(graphql, request_id).await?;
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{request_id}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
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
        request_doc_id = escape_graphql_string(&request_doc_id),
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
    let (request_doc_id, _, requester_did) = exact_request_binding(graphql, request_id).await?;
    anyhow::ensure!(requester_did.as_deref() == Some(agent_did));
    let message_key = format!("{session_id}:2");
    let content = r#"{"role":"assistant","id":null,"content":[{"text":"durable child answer"}]}"#;
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
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
        request_doc_id = escape_graphql_string(&request_doc_id),
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
