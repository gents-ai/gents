use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_streams_claimed_background_completion_and_replays_it_once() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("background-wake-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-background-wake-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-background-wake-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let identity = identity_from_init(&init)?;
    let behavior_id = format!("{agent_did}:default");
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
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
    let wake_request_id =
        seed_background_completion_wake(&graphql, &identity, &behavior_id, &thread_id).await?;

    let started = tokio::time::timeout(Duration::from_secs(30), read_turn_started(&mut ws))
        .await
        .context("background completion wake was not projected as a new turn")??;
    assert_eq!(started.thread_id, thread_id);
    assert_eq!(started.turn.id, wake_request_id);
    let capture = tokio::time::timeout(Duration::from_secs(30), read_turn_capture(&mut ws))
        .await
        .context("background completion wake did not finish in the connected client")??;
    assert_eq!(capture.turn.id, wake_request_id);
    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&expected_reply),
        "missing background continuation output: {}",
        capture.text
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(105),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let resumed: codex::ThreadResumeResponse =
        read_typed_response(&mut ws, request_id(105)).await?;
    let wake_turns = resumed
        .thread
        .turns
        .iter()
        .filter(|turn| turn.id == wake_request_id)
        .collect::<Vec<_>>();
    assert_eq!(wake_turns.len(), 1, "wake replay must not duplicate turns");
    assert_turn_has_agent_text(wake_turns[0], &expected_reply);
    assert!(
        !wake_turns[0]
            .items
            .iter()
            .any(|item| matches!(item, codex::ThreadItem::UserMessage { .. })),
        "internal background wake prompt must not replay as user input"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_resume_finishes_an_in_progress_background_completion() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("resumed-background-wake-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-resumed-background-wake-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_routed_delayed(
        &model_name,
        Vec::new(),
        expected_reply.clone(),
        Duration::from_secs(5),
    )?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-resumed-background-wake-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let identity = identity_from_init(&init)?;
    let behavior_id = format!("{agent_did}:default");
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
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
    let wake_request_id =
        seed_background_completion_wake(&graphql, &identity, &behavior_id, &thread_id).await?;

    let started = tokio::time::timeout(Duration::from_secs(30), read_turn_started(&mut ws))
        .await
        .context("background completion wake was not projected as a new turn")??;
    assert_eq!(started.turn.id, wake_request_id);
    assert_eq!(started.turn.status, codex::TurnStatus::InProgress);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(106),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let resumed: codex::ThreadResumeResponse =
        read_typed_response(&mut ws, request_id(106)).await?;
    let baseline = resumed
        .thread
        .turns
        .iter()
        .find(|turn| turn.id == wake_request_id)
        .context("resume omitted the in-progress background wake")?;
    assert_eq!(baseline.status, codex::TurnStatus::InProgress);

    let capture = tokio::time::timeout(Duration::from_secs(30), read_turn_capture(&mut ws))
        .await
        .context("resumed background wake did not finish in the connected client")??;
    assert_eq!(capture.turn.id, wake_request_id);
    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&expected_reply),
        "missing resumed background continuation output: {}",
        capture.text
    );

    Ok(())
}
