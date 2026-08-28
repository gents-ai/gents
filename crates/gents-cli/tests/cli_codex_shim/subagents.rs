use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_projects_authorized_subagent_and_enforces_read_only_child_thread() -> Result<()>
{
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-subagent-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-subagent-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let behavior_id = format!("{agent_did}:default");
    let child_behavior_id = format!("{behavior_id}:reviewer");
    let child_backend_id = "child-projection-backend";
    let child_model_name = "child-projection-model";
    let child_model_selection = format!("{child_backend_id}::{child_model_name}");
    let root_model_selection =
        gents_model_selection_id(&default_backend_id(&agent_did), &model_name);
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "50",
            "--codex-shim-timeout-secs",
            "60",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let parent_thread_id = start_thread(&mut ws, &home_dir).await?;
    let prompt = format!("hold subagent projection {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(230),
            params: codex::TurnStartParams {
                thread_id: parent_thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(230)))
        .await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);
    let parent_active = read_thread_status_changed(&mut ws, &parent_thread_id).await?;
    assert!(matches!(parent_active, codex::ThreadStatus::Active { .. }));
    let (parent_request_id, session_id, _) =
        wait_for_request(&graphql, &agent_did, &prompt).await?;
    assert_eq!(session_id, parent_thread_id);

    let child_thread_id = Uuid::new_v4().to_string();
    let child_request_id = Uuid::new_v4().to_string();
    let tool_call_id = format!("spawn-{}", Uuid::new_v4().simple());
    let tool_call_key = format!("{parent_thread_id}:{tool_call_id}");
    seed_authorized_subagent_link(
        &graphql,
        &agent_did,
        &child_behavior_id,
        &parent_request_id,
        &parent_thread_id,
        &child_request_id,
        &child_thread_id,
        &tool_call_id,
        &tool_call_key,
        child_backend_id,
        child_model_name,
    )
    .await?;

    let (running, projected_model, reasoning_effort_absent) = tokio::time::timeout(
        Duration::from_secs(15),
        read_collab_agent_status(&mut ws, &tool_call_key, &child_thread_id),
    )
    .await
    .context("timed out waiting for native subagent projection")??;
    assert_eq!(running, codex::CollabAgentStatus::Running);
    assert_eq!(
        projected_model.as_deref(),
        Some(child_model_selection.as_str())
    );
    assert!(reasoning_effort_absent);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(239),
            params: codex::ThreadListParams {
                cursor: None,
                limit: None,
                sort_key: None,
                sort_direction: None,
                model_providers: None,
                source_kinds: Some(vec![codex::ThreadSourceKind::SubAgentThreadSpawn]),
                archived: None,
                cwd: None,
                use_state_db_only: true,
                search_term: None,
            },
        },
    )
    .await?;
    let subagent_list: codex::ThreadListResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(239)))
        .await?;
    assert_eq!(subagent_list.data.len(), 1, "{subagent_list:?}");
    assert_eq!(subagent_list.data[0].id, child_thread_id);
    assert!(matches!(
        subagent_list.data[0].source,
        codex::SessionSource::SubAgent(_)
    ));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(231),
            params: codex::ThreadReadParams {
                thread_id: child_thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let child_read: codex::ThreadReadResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(231)))
        .await?;
    assert_eq!(child_read.thread.id, child_thread_id);
    assert!(matches!(
        child_read.thread.status,
        codex::ThreadStatus::Active { .. }
    ));
    assert_eq!(child_read.thread.turns.len(), 1);
    assert_eq!(
        child_read.thread.turns[0].status,
        codex::TurnStatus::InProgress
    );
    let child_json = serde_json::to_value(&child_read.thread)?;
    assert_eq!(
        child_json.pointer("/source/subAgent/thread_spawn/parent_thread_id"),
        Some(&Value::String(parent_thread_id.clone()))
    );
    assert!(child_json.get("parentThreadId").is_none());

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(232),
            params: codex::ThreadResumeParams {
                thread_id: child_thread_id.clone(),
                cwd: None,
                ..Default::default()
            },
        },
    )
    .await?;
    let child_resume: codex::ThreadResumeResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(232)))
        .await?;
    assert_eq!(child_resume.thread.id, child_thread_id);
    assert_eq!(child_resume.model, child_model_selection);

    delete_agent_behavior(&graphql, &child_behavior_id).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(241),
            params: codex::ThreadResumeParams {
                thread_id: child_thread_id.clone(),
                cwd: None,
                ..Default::default()
            },
        },
    )
    .await?;
    let child_resume_without_behavior: codex::ThreadResumeResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(241)))
        .await?;
    assert_eq!(child_resume_without_behavior.thread.id, child_thread_id);
    assert_eq!(child_resume_without_behavior.model, root_model_selection);

    let live_child_text = format!("live child output {}", Uuid::new_v4().simple());
    let live_child_reasoning = format!("child reasoning {}", Uuid::new_v4().simple());
    let child_started_at_ms = seed_child_streaming_response(
        &graphql,
        &agent_did,
        &child_behavior_id,
        &child_request_id,
        &child_thread_id,
        &live_child_text,
        &live_child_reasoning,
    )
    .await?;
    let (projected_text, projected_reasoning, projected_started_at_ms) = match tokio::time::timeout(
        Duration::from_secs(15),
        read_child_agent_and_reasoning_deltas(&mut ws, &child_thread_id, &child_request_id),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            let (stdout, stderr) = serve.captured_output()?;
            bail!(
                "timed out waiting for live loaded-child delta\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
    };
    assert_eq!(projected_text, live_child_text);
    assert_eq!(projected_reasoning, live_child_reasoning);
    assert_eq!(projected_started_at_ms, child_started_at_ms);

    // End the parent turn while the child response is still streaming. From
    // this point onward the connection-scoped root and loaded-child watchers,
    // rather than the parent turn stream, own the native card and child deltas.
    let unresolved_tool_call_key = format!("{parent_thread_id}:unresolved-spawn");
    let unresolved_completed_at_ms = seed_unresolved_completed_subagent_tool(
        &graphql,
        &agent_did,
        &parent_request_id,
        &parent_thread_id,
        &unresolved_tool_call_key,
    )
    .await?;
    update_request_lifecycle(&graphql, &parent_request_id, "failed").await?;
    tokio::time::timeout(
        Duration::from_secs(15),
        read_mcp_tool_completion(
            &mut ws,
            &unresolved_tool_call_key,
            unresolved_completed_at_ms,
        ),
    )
    .await
    .context("timed out waiting for terminal unresolved subagent MCP fallback")??;

    let parent_failed = tokio::time::timeout(
        Duration::from_secs(15),
        read_thread_status_changed(&mut ws, &parent_thread_id),
    )
    .await
    .context("timed out waiting for failed root thread status")??;
    assert_eq!(parent_failed, codex::ThreadStatus::SystemError);

    let reasoning_tail = " then checks the durable result";
    update_streaming_response_reasoning(
        &graphql,
        &child_request_id,
        &format!("{live_child_reasoning}{reasoning_tail}"),
        2,
    )
    .await?;
    let appended_reasoning = tokio::time::timeout(
        Duration::from_secs(15),
        read_child_reasoning_delta(&mut ws, &child_thread_id, &child_request_id),
    )
    .await
    .context("timed out waiting for appended child reasoning delta")??;
    assert_eq!(appended_reasoning, reasoning_tail);

    let durable_reasoning = format!("{live_child_reasoning}{reasoning_tail}");
    let child_materialized_at_ms = materialize_child_response_before_terminal(
        &graphql,
        &agent_did,
        &child_request_id,
        &child_thread_id,
        &durable_reasoning,
    )
    .await?;
    let (completed_reasoning, projected_materialized_at_ms) = tokio::time::timeout(
        Duration::from_secs(15),
        read_child_reasoning_completion(&mut ws, &child_thread_id, &child_request_id),
    )
    .await
    .context("timed out waiting for reasoning completion at the final reset-tail window")??;
    assert_eq!(completed_reasoning, durable_reasoning);
    assert_eq!(projected_materialized_at_ms, child_materialized_at_ms);

    let child_completed_at_ms =
        finalize_child_response_after_materialization(&graphql, &child_request_id).await?;
    let (completed, child_thread_status, projected_completed_at_ms) = tokio::time::timeout(
        Duration::from_secs(15),
        read_terminal_child_without_reasoning_replay(
            &mut ws,
            &child_thread_id,
            &child_request_id,
            &tool_call_key,
        ),
    )
    .await
    .context("timed out waiting for terminal child state after final reset-tail window")??;
    assert_eq!(completed, codex::CollabAgentStatus::Completed);
    assert_eq!(child_thread_status, codex::ThreadStatus::Idle);
    assert_eq!(projected_completed_at_ms, child_completed_at_ms);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(240),
            params: codex::ThreadReadParams {
                thread_id: child_thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let completed_child: codex::ThreadReadResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(240)))
        .await?;
    assert!(completed_child.thread.turns.iter().any(|turn| {
        turn.items.iter().any(|item| {
            matches!(
                item,
                codex::ThreadItem::Reasoning { summary, content, .. }
                    if summary.is_empty()
                        && content.len() == 1
                        && content.first().is_some_and(|text| text == &durable_reasoning)
            )
        })
    }));

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(233),
            params: codex::TurnStartParams {
                thread_id: child_thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "must be rejected".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let start_error = read_error_response(&mut ws, request_id(233)).await?;
    assert!(start_error.message.contains("read-only"));

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(234),
            params: codex::TurnSteerParams {
                thread_id: child_thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "must also be rejected".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: child_request_id.clone(),
            },
        },
    )
    .await?;
    let steer_error = read_error_response(&mut ws, request_id(234)).await?;
    assert!(steer_error.message.contains("read-only"));

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(235),
            params: codex::TurnInterruptParams {
                thread_id: child_thread_id,
                turn_id: child_request_id,
            },
        },
    )
    .await?;
    let interrupt_error = read_error_response(&mut ws, request_id(235)).await?;
    assert!(interrupt_error.message.contains("read-only"));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(242),
            params: codex::ThreadReadParams {
                thread_id: parent_thread_id.clone(),
                include_turns: false,
            },
        },
    )
    .await?;
    let failed_parent: codex::ThreadReadResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(242)))
        .await?;
    assert_eq!(
        failed_parent.thread.status,
        codex::ThreadStatus::SystemError
    );

    Ok(())
}
