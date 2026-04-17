pub(crate) async fn insert_agent_principal(
    core: &ClientCore,
    agent_did: &str,
    display_name: &str,
    default_behavior_id: &str,
) -> Result<()> {
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            add_AgentPrincipal(input: {{
                agent_did: "{agent_did}"
                display_name: "{display_name}"
                default_behavior_id: "{default_behavior_id}"
                enabled: true
            }}) {{ agent_did }}
        }}"#,
            agent_did = escape_graphql_string(agent_did),
            display_name = escape_graphql_string(display_name),
            default_behavior_id = escape_graphql_string(default_behavior_id),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("add_AgentPrincipal failed: {:?}", response.errors);
    }
    Ok(())
}

pub(crate) async fn insert_agent_runtime(
    core: &ClientCore,
    agent_did: &str,
    default_behavior_id: &str,
) -> Result<()> {
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            upsert_AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    agent_did: "{agent_did}"
                    process_state: "ready"
                    reconcile_phase: "idle"
                    active_generation: 1
                    router_generation: 1
                    default_behavior_id: "{default_behavior_id}"
                    runnable_behavior_count: 1
                    unavailable_behavior_count: 0
                    last_reconcile_result: "startup"
                    last_reconcile_error: ""
                    last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                    updated_at: "2026-04-14T00:00:00Z"
                }},
                update: {{
                    process_state: "ready"
                    reconcile_phase: "idle"
                    active_generation: 1
                    router_generation: 1
                    default_behavior_id: "{default_behavior_id}"
                    runnable_behavior_count: 1
                    unavailable_behavior_count: 0
                    last_reconcile_result: "startup"
                    last_reconcile_error: ""
                    last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                    updated_at: "2026-04-14T00:00:00Z"
                }}
            ) {{ _docID }}
        }}"#,
            agent_did = escape_graphql_string(agent_did),
            default_behavior_id = escape_graphql_string(default_behavior_id),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("upsert_AgentRuntime failed: {:?}", response.errors);
    }
    Ok(())
}

pub(crate) async fn seed_operator_documents(core: &ClientCore) -> Result<()> {
    insert_agent_principal(core, "did:defra:amy", "Amy", "amy-default").await?;
    insert_agent_runtime(core, "did:defra:amy", "amy-default").await?;

    core.save_backend(&InferenceBackendRow {
        backend_id: "backend-amy".to_string(),
        name: Some("OpenRouter".to_string()),
        provider_kind: Some("openrouter".to_string()),
        endpoint: Some("https://openrouter.ai/api/v1".to_string()),
        api_key: None,
        api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
        max_concurrent: Some(2),
        max_queue_depth: Some(100),
        enabled: Some(true),
        models: vec!["openai/gpt-5.4".to_string()],
        last_probe: None,
        probe_status: Some("healthy".to_string()),
    })
    .await?;
    core.save_inference_profile(&InferenceProfileRow {
        profile_id: "profile-amy".to_string(),
        display_name: Some("Amy Profile".to_string()),
        context_window: Some(128000),
        max_output_tokens: Some(4096),
        max_turns: Some(24),
        temperature: Some(0.2),
        stream_batch_ms: Some(50),
        deadline_duration_secs: Some(300),
    })
    .await?;
    core.save_tool_selection(&ToolSelectionRow {
        selection_id: "tools-amy".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Tools".to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some("workspace-write".to_string()),
        enable_bash: Some(true),
        bash_mode: Some("workspace".to_string()),
        cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
        enable_meta_tools: Some(true),
        delegate_to: vec!["planner".to_string()],
    })
    .await?;
    core.save_behavior(&AgentBehaviorRow {
        behavior_id: "amy-default".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Default".to_string()),
        system_prompt: Some("You are Amy.".to_string()),
        backend_id: Some("backend-amy".to_string()),
        model_name: Some("openai/gpt-5.4".to_string()),
        tool_selection_id: Some("tools-amy".to_string()),
        inference_profile_id: Some("profile-amy".to_string()),
        compaction_strategy: Some("StripThenSummarize".to_string()),
        compaction_threshold: Some(0.7),
        enabled: Some(true),
        created_at: Some("2026-04-14T00:00:00Z".to_string()),
    })
    .await?;
    core.save_scheduled_task(&ScheduledTaskRow {
        task_id: "task-amy-daily".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        name: Some("Daily Amy".to_string()),
        prompt: Some("Check the daily queue.".to_string()),
        interval_secs: Some(300),
        enabled: Some(true),
        next_run_at: Some("2026-04-15T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: Some("ok".to_string()),
        last_error: None,
        run_count: Some(4),
        created_at: None,
        updated_at: None,
    })
    .await?;
    core.refresh_store().await?;
    Ok(())
}

pub(crate) async fn insert_chat_transcript_documents(
    core: &ClientCore,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    response_key: &str,
) -> Result<()> {
    let response_content = "Queue checked.\n\n- Found the audit target.\n- Ready to continue.";
    let response_reasoning =
        "I verified the latest request, ran the shell tool, and summarized the result.";
    let assistant_tool_call_message = serde_json::to_string(&Message::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::ToolCall(ToolCall {
                id: "call-shell-1".to_string(),
                call_id: Some("call-shell-1".to_string()),
                function: ToolFunction {
                    name: "shell".to_string(),
                    arguments: serde_json::json!({ "cmd": "rg audit" }),
                },
                signature: None,
                additional_params: None,
            }),
            AssistantContent::Text(Text {
                text: "I checked the queue and opened the trace.".to_string(),
            }),
        ])
        .context("assistant tool-call content")?,
    })?;
    let tool_result_message = serde_json::to_string(&Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: "call-shell-1".to_string(),
            call_id: Some("call-shell-1".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: "src/app.rs: audit target live".to_string(),
            })),
        })),
    })?;
    let assistant_final_message = serde_json::to_string(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: "Queue checked.\n\n- Found the audit target.\n- Ready to continue.".to_string(),
        })),
    })?;

    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            add_AgentMessage(input: {{
                message_key: "msg-assistant-1"
                session_id: "{session_id}"
                sequence: 2
                role: "assistant"
                content: "{assistant_tool_call_message}"
                timestamp: "2026-04-14T00:00:01Z"
            }}) {{ message_key }}
            add_AgentMessage(input: {{
                message_key: "msg-tool-result-1"
                session_id: "{session_id}"
                sequence: 3
                role: "user"
                content: "{tool_result_message}"
                timestamp: "2026-04-14T00:00:03Z"
            }}) {{ message_key }}
            add_AgentMessage(input: {{
                message_key: "msg-assistant-2"
                session_id: "{session_id}"
                sequence: 4
                role: "assistant"
                content: "{assistant_final_message}"
                timestamp: "2026-04-14T00:00:04Z"
            }}) {{ message_key }}
            add_AgentToolCall(input: {{
                tool_call_key: "tool-call-1"
                session_id: "{session_id}"
                message_sequence: 2
                tool_name: "shell"
                tool_call_id: "call-shell-1"
                args: "{{\"cmd\":\"rg audit\"}}"
                status: "completed"
                started_at: "2026-04-14T00:00:02Z"
                completed_at: "2026-04-14T00:00:03Z"
            }}) {{ tool_call_key }}
            add_AgentToolResult(input: {{
                agent_did: "{agent_did}"
                session_id: "{session_id}"
                tool_name: "shell"
                tool_input: "rg audit"
                output_text: "src/app.rs: audit target live"
                truncated: false
                truncation_metadata: ""
                conversation_doc_id: "{session_id}"
                created_at: "2026-04-14T00:00:03Z"
            }}) {{ _docID }}
            add_AgentResponse(input: {{
                response_key: "{response_key}"
                agent_did: "{agent_did}"
                behavior_id: "{behavior_id}"
                session_id: "{session_id}"
                content: "{response_content}"
                reasoning: "{response_reasoning}"
                status: "completed"
                error_message: ""
                token_count: 42
                progress_seq: 1
                created_at: "2026-04-14T00:00:04Z"
                completed_at: "2026-04-14T00:00:05Z"
            }}) {{ response_key }}
        }}"#,
            session_id = escape_graphql_string(session_id),
            agent_did = escape_graphql_string(agent_did),
            behavior_id = escape_graphql_string(behavior_id),
            response_key = escape_graphql_string(response_key),
            assistant_tool_call_message = escape_graphql_string(&assistant_tool_call_message),
            tool_result_message = escape_graphql_string(&tool_result_message),
            assistant_final_message = escape_graphql_string(&assistant_final_message),
            response_content = escape_graphql_string(response_content),
            response_reasoning = escape_graphql_string(response_reasoning),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "insert chat transcript documents failed: {:?}",
            response.errors
        );
    }
    core.refresh_store().await?;
    Ok(())
}

pub(crate) async fn query_has_row_by_unique_field(
    core: &ClientCore,
    root: &str,
    field: &str,
    value: &str,
) -> Result<bool> {
    let escaped_value = escape_graphql_string(value);
    let query = format!(
        r#"{{
            {root}(
                filter: {{ {field}: {{ _eq: "{escaped_value}" }} }},
                limit: 1
            ) {{
                {field}
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query {root}.{field}={value} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(root))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        == Some(value))
}

pub(crate) async fn seed_live_operator_documents(
    core: &ClientCore,
    agent_did: &str,
    agent_name: &str,
    backend: &AgentBackendConfig,
) -> Result<LiveAgentDocs> {
    let behavior_id = default_behavior_id_for_agent(agent_did);
    let backend_id = format!("{agent_name}-backend");
    let tool_selection_id = format!("{behavior_id}:tools");
    let inference_profile_id = format!("{behavior_id}:profile");
    let scheduled_task_id = format!("{behavior_id}:scheduled-task");

    core.save_tool_selection(&ToolSelectionRow {
        selection_id: tool_selection_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Audit Tools".to_string()),
        enable_file_tools: Some(false),
        file_tools_mode: Some("readonly".to_string()),
        enable_bash: Some(false),
        bash_mode: Some("disabled".to_string()),
        cli_tool_names: vec![],
        enable_meta_tools: Some(false),
        delegate_to: vec![],
    })
    .await?;
    core.save_inference_profile(&InferenceProfileRow {
        profile_id: inference_profile_id.clone(),
        display_name: Some("Live Audit Profile".to_string()),
        context_window: Some(131072),
        max_output_tokens: Some(1024),
        max_turns: Some(12),
        temperature: Some(0.0),
        stream_batch_ms: Some(50),
        deadline_duration_secs: Some(300),
    })
    .await?;
    core.save_behavior(&AgentBehaviorRow {
        behavior_id: behavior_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Audit Default".to_string()),
        system_prompt: Some(
            "You are a terse desktop integration test agent. Follow exact reply instructions."
                .to_string(),
        ),
        backend_id: Some(backend_id.clone()),
        model_name: Some(backend.model_name.clone()),
        tool_selection_id: Some(tool_selection_id.clone()),
        inference_profile_id: Some(inference_profile_id.clone()),
        compaction_strategy: Some("StripThenSummarize".to_string()),
        compaction_threshold: Some(0.95),
        enabled: Some(true),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    })
    .await?;
    core.save_scheduled_task(&ScheduledTaskRow {
        task_id: scheduled_task_id.clone(),
        agent_did: Some(agent_did.to_string()),
        behavior_id: Some(behavior_id.clone()),
        name: Some("Live Audit Scheduled Task".to_string()),
        prompt: Some("Summarize the live audit queue.".to_string()),
        interval_secs: Some(3600),
        enabled: Some(true),
        next_run_at: Some("2035-01-01T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: Some("ok".to_string()),
        last_error: None,
        run_count: Some(0),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    })
    .await?;
    core.refresh_store().await?;

    Ok(LiveAgentDocs {
        behavior_id,
        backend_id,
        tool_selection_id,
        inference_profile_id,
        scheduled_task_id,
    })
}

