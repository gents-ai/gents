use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_steer_queues_gents_request_on_active_turn() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-steer-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-steer-{}", Uuid::new_v4().simple());
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
            "100",
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
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let initial_prompt = format!("hold the turn open {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(201),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: initial_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(201)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(202),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "wrong expected turn".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: "stale-turn".to_string(),
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(202)).await?;
    assert_eq!(
        error.message,
        format!(
            "expected active turn id `stale-turn` but found `{}`",
            turn_start.turn.id
        )
    );

    let steer_prompt = format!("steer while active {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(203),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let steer: codex::TurnSteerResponse = read_typed_response(&mut ws, request_id(203)).await?;
    assert_eq!(steer.turn_id, turn_start.turn.id);

    let (steering_request_id, session_id, metadata) =
        wait_for_request_metadata(&graphql, &agent_did, &steer_prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_eq!(
        metadata.pointer("/queue/source").and_then(Value::as_str),
        Some("steering")
    );
    assert_eq!(
        metadata.pointer("/queue/policy").and_then(Value::as_str),
        Some("append")
    );
    assert_eq!(
        metadata
            .pointer("/queue/queued_after_request_id")
            .and_then(Value::as_str),
        Some(turn_start.turn.id.as_str())
    );
    assert_ne!(steering_request_id, turn_start.turn.id);

    let second_steer_prompt = format!("second steer while active {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(205),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: second_steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let second_steer: codex::TurnSteerResponse =
        read_typed_response(&mut ws, request_id(205)).await?;
    assert_eq!(second_steer.turn_id, turn_start.turn.id);

    let (second_steering_request_id, second_session_id, second_metadata) =
        wait_for_request_metadata(&graphql, &agent_did, &second_steer_prompt).await?;
    assert_eq!(second_session_id, thread_id);
    assert_eq!(
        second_metadata
            .pointer("/queue/queued_after_request_id")
            .and_then(Value::as_str),
        Some(steering_request_id.as_str()),
        "second steering request should queue after the current GENTS tail, not after the root turn"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(204),
            params: codex::TurnInterruptParams {
                thread_id,
                turn_id: turn_start.turn.id,
            },
        },
    )
    .await?;
    let _: codex::TurnInterruptResponse = read_typed_response(&mut ws, request_id(204)).await?;
    wait_for_request_lifecycle_state(
        &graphql,
        &steering_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;
    wait_for_request_lifecycle_state(
        &graphql,
        &second_steering_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_interrupt_completes_with_running_background_tool() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-bg-interrupt-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-bg-interrupt-{}", Uuid::new_v4().simple());
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
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let prompt = format!(
        "start background interrupt repro {}",
        Uuid::new_v4().simple()
    );
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(220),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(220)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);

    let (gents_request_id, session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    let tool_call_key = format!("{session_id}:codex-bg-interrupt");
    seed_running_background_tool(&graphql, &gents_request_id, &session_id, &tool_call_key).await?;

    let started_process = tokio::time::timeout(
        Duration::from_secs(15),
        read_background_command_started(&mut ws, &tool_call_key),
    )
    .await
    .context("timed out waiting for shim to project running background tool")??;
    assert_eq!(started_process, tool_call_key);

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(221),
            params: codex::TurnInterruptParams {
                thread_id: thread_id.clone(),
                turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let interrupted_turn = tokio::time::timeout(
        Duration::from_secs(15),
        read_interrupt_response_and_completed_turn(&mut ws, request_id(221)),
    )
    .await
    .context("timed out waiting for interrupted turn with running background tool")??;
    assert_eq!(interrupted_turn.status, codex::TurnStatus::Interrupted);

    wait_for_request_lifecycle_state(
        &graphql,
        &gents_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(222),
            params: codex::ThreadListParams {
                cursor: None,
                limit: None,
                sort_key: None,
                sort_direction: None,
                model_providers: None,
                source_kinds: None,
                archived: None,
                cwd: None,
                use_state_db_only: true,
                search_term: None,
            },
        },
    )
    .await?;
    let _: codex::ThreadListResponse = tokio::time::timeout(
        Duration::from_secs(15),
        read_typed_response(&mut ws, request_id(222)),
    )
    .await
    .context("shim stopped answering after interrupting background-tool turn")??;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_steer_drains_queued_request_before_completing_turn() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-steer-drain-{}", Uuid::new_v4().simple());
    let initial_prompt = format!("first active turn {}", Uuid::new_v4().simple());
    let steer_prompt = format!("queued steering {}", Uuid::new_v4().simple());
    let first_reply = format!("first-drain-{}", Uuid::new_v4().simple());
    let second_reply = format!("second-drain-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_routed_delayed(
        &model_name,
        vec![
            (steer_prompt.clone(), second_reply.clone()),
            (initial_prompt.clone(), first_reply.clone()),
        ],
        "steer-drain-title".to_string(),
        Duration::from_millis(750),
    )?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-steer-drain-{}", Uuid::new_v4().simple());
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
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(301),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: initial_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(301)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(302),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let steer: codex::TurnSteerResponse = read_typed_response(&mut ws, request_id(302)).await?;
    assert_eq!(steer.turn_id, turn_start.turn.id);

    let capture = read_turn_capture(&mut ws).await?;
    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&first_reply),
        "turn completed before streaming first reply {first_reply}; text:\n{}",
        capture.text
    );
    assert!(
        capture.text.contains(&second_reply),
        "turn completed before draining steering reply {second_reply}; text:\n{}",
        capture.text
    );

    let (_initial_request_id, initial_session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &initial_prompt).await?;
    assert_eq!(initial_session_id, thread_id);
    let (steering_request_id, steering_session_id, metadata) =
        wait_for_request_metadata(&graphql, &agent_did, &steer_prompt).await?;
    assert_eq!(steering_session_id, thread_id);
    assert_ne!(steering_request_id, turn_start.turn.id);
    assert_eq!(
        metadata.pointer("/queue/source").and_then(Value::as_str),
        Some("steering")
    );
    assert_eq!(
        metadata
            .pointer("/queue/queued_after_request_id")
            .and_then(Value::as_str),
        Some(turn_start.turn.id.as_str())
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &initial_prompt)),
        "mock endpoint did not receive the initial prompt; captured={captured_requests:?}"
    );
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &steer_prompt)),
        "mock endpoint did not receive the steering prompt; captured={captured_requests:?}"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(303),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let thread_history: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(303)).await?;
    assert_eq!(
        thread_history.thread.turns.len(),
        1,
        "queued steering should reload as one Codex turn"
    );
    let history_turn = &thread_history.thread.turns[0];
    assert_eq!(history_turn.id, turn_start.turn.id);
    assert_turn_has_user_text(history_turn, &initial_prompt);
    assert_turn_has_agent_text(history_turn, &first_reply);
    assert_turn_has_user_text(history_turn, &steer_prompt);
    assert_turn_has_agent_text(history_turn, &second_reply);

    Ok(())
}
