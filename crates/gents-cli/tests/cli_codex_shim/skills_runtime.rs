use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_live_skill_add_reaches_model_in_conversation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("skill-live-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-skill-live-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-live-{}", Uuid::new_v4().simple());
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

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
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
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let catalog_phrase = format!("cite-sources-{}", Uuid::new_v4().simple());
    run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "add",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--skill-id",
            "live-skill",
            "--scope",
            "principal",
            "--name",
            &catalog_phrase,
            "--description",
            "find and cite sources",
            "--instructions",
            "Always cite your sources.",
        ],
    )?;
    wait_for_runtime_quiescence(&graphql, &agent_did, gen0 + 1, Duration::from_secs(2)).await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(2),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(2)).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(3),
            params: codex::TurnStartParams {
                thread_id: thread_start.thread.id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(3)).await?;
    let (_text, completed) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed.status, codex::TurnStatus::Completed);

    let captured = mock_endpoint.captured_chat_requests();
    assert!(
        captured.iter().any(|request| {
            let text = request.to_string();
            text.contains(&catalog_phrase) && text.contains("load_skill")
        }),
        "live-added skill's catalog entry did not reach the model; captured={captured:?}"
    );

    let _ = ws.close(None).await;
    Ok(())
}

/// A committed `skills/config/write` toggle wakes reconciliation so the next
/// turn uses the updated skill catalog without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_live_skill_toggle_reaches_model_in_conversation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("skill-toggle-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-skill-toggle-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-toggle-{}", Uuid::new_v4().simple());
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

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
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
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let catalog_phrase = format!("toggle-cite-{}", Uuid::new_v4().simple());
    run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "add",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--skill-id",
            "toggle-skill",
            "--scope",
            "principal",
            "--name",
            &catalog_phrase,
            "--description",
            "find and cite sources",
            "--instructions",
            "Always cite your sources.",
        ],
    )?;
    let gen1 =
        wait_for_runtime_quiescence(&graphql, &agent_did, gen0 + 1, Duration::from_secs(2)).await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(2),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(2)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(3),
            params: codex::TurnStartParams {
                thread_id: thread_start.thread.id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(3)).await?;
    let (_text, completed) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed.status, codex::TurnStatus::Completed);
    assert!(
        mock_endpoint
            .captured_chat_requests()
            .iter()
            .any(|request| {
                let text = request.to_string();
                text.contains(&catalog_phrase) && text.contains("load_skill")
            }),
        "enabled skill's catalog entry should reach the model before the disable"
    );

    let captured_before_toggle = mock_endpoint.captured_chat_requests().len();
    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsConfigWrite {
            request_id: request_id(4),
            params: codex::SkillsConfigWriteParams {
                path: None,
                name: Some(catalog_phrase.clone()),
                enabled: false,
            },
        },
    )
    .await?;
    let write: codex::SkillsConfigWriteResponse =
        read_typed_response(&mut ws, request_id(4)).await?;
    assert!(
        !write.effective_enabled,
        "shim should report the skill disabled"
    );

    wait_for_runtime_quiescence(&graphql, &agent_did, gen1 + 1, Duration::from_secs(2)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(5),
            params: codex::TurnStartParams {
                thread_id: thread_start.thread.id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "hello again".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(5)).await?;
    let (_text, completed) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed.status, codex::TurnStatus::Completed);

    let captured = mock_endpoint.captured_chat_requests();
    assert!(
        captured.len() > captured_before_toggle,
        "turn 2 should have produced at least one new captured request"
    );
    assert!(
        captured[captured_before_toggle..]
            .iter()
            .all(|request| !request.to_string().contains(&catalog_phrase)),
        "disabled skill's catalog entry must NOT reach the model after the shim toggle reconciled; \
         captured tail={:?}",
        &captured[captured_before_toggle..]
    );

    let _ = ws.close(None).await;
    Ok(())
}
