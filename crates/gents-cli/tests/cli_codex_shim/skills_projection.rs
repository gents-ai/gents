use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_lists_and_toggles_skills() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-skill-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "ok")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-skill-{}", Uuid::new_v4().simple());
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
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let added = run_cli_json(
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
            "research",
            "--scope",
            "principal",
            "--name",
            "Research",
            "--description",
            "Find and cite sources",
            "--instructions",
            "Always cite your sources.",
        ],
    )?;
    assert_eq!(
        added.get("skill_id").and_then(Value::as_str),
        Some("research")
    );
    let foreign_agent_did = "did:key:zForeignSkillOwner";
    run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "add",
            "--graphql",
            &graphql,
            "--agent-did",
            foreign_agent_did,
            "--skill-id",
            "foreign-skill",
            "--scope",
            "principal",
            "--name",
            "Foreign",
            "--instructions",
            "Must remain enabled.",
        ],
    )?;

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
        codex::ClientRequest::SkillsList {
            request_id: request_id(2),
            params: codex::SkillsListParams::default(),
        },
    )
    .await?;
    let list: codex::SkillsListResponse = read_typed_response(&mut ws, request_id(2)).await?;
    let research = list
        .data
        .iter()
        .flat_map(|entry| entry.skills.iter())
        .find(|skill| skill.name == "Research")
        .expect("Research skill should be listed");
    assert!(research.enabled, "newly added skill should be enabled");
    assert_eq!(research.scope, codex::SkillScope::System);

    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsConfigWrite {
            request_id: request_id(3),
            params: codex::SkillsConfigWriteParams {
                path: Some(std::path::PathBuf::from("/gents/skills/foreign-skill").try_into()?),
                name: None,
                enabled: false,
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(3)).await?;
    assert!(
        error
            .message
            .contains("no skill \"foreign-skill\" belongs to bound agent"),
        "unexpected cross-agent toggle error: {}",
        error.message
    );
    let foreign = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "foreign-skill",
        ],
    )?;
    assert_eq!(
        foreign.get("agent_did").and_then(Value::as_str),
        Some(foreign_agent_did)
    );
    assert_eq!(foreign.get("enabled").and_then(Value::as_bool), Some(true));

    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsConfigWrite {
            request_id: request_id(4),
            params: codex::SkillsConfigWriteParams {
                path: None,
                name: Some("Research".to_string()),
                enabled: false,
            },
        },
    )
    .await?;
    let write: codex::SkillsConfigWriteResponse =
        read_typed_response(&mut ws, request_id(4)).await?;
    assert!(
        !write.effective_enabled,
        "config write should report disabled"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsList {
            request_id: request_id(5),
            params: codex::SkillsListParams::default(),
        },
    )
    .await?;
    let list: codex::SkillsListResponse = read_typed_response(&mut ws, request_id(5)).await?;
    let research = list
        .data
        .iter()
        .flat_map(|entry| entry.skills.iter())
        .find(|skill| skill.name == "Research")
        .expect("Research skill should still be listed");
    assert!(
        !research.enabled,
        "skill should be disabled after skills/config/write"
    );

    let _ = ws.close(None).await;
    Ok(())
}

/// An explicit Codex skill selection resolves through the runtime's effective
/// set and injects the body even when the turn contains no text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_explicit_skill_selection_injects_body_into_turn() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("skill-inject-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-skill-inject-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-inject-{}", Uuid::new_v4().simple());
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
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let body_phrase = format!("INJECTED-BODY-{}", Uuid::new_v4().simple());
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
            "inject-skill",
            "--scope",
            "principal",
            "--name",
            "Injectable",
            "--description",
            "a skill to inject",
            "--instructions",
            &body_phrase,
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
                input: vec![codex::UserInput::Skill {
                    name: "Injectable".to_string(),
                    path: std::path::PathBuf::from("/gents/skills/inject-skill"),
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
            text.contains(&body_phrase) && text.contains("system-reminder")
        }),
        "explicit skill selection did not inject the body; captured={captured:?}"
    );

    let _ = ws.close(None).await;
    Ok(())
}

/// A selection outside the bound behavior's effective set must not inject its
/// skill body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_explicit_selection_respects_effective_set() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("scope-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-skill-scope-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-scope-{}", Uuid::new_v4().simple());
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
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let body_phrase = format!("UNSCOPED-BODY-{}", Uuid::new_v4().simple());
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
            "unscoped-skill",
            "--scope",
            "behavior",
            "--name",
            "Unscoped",
            "--description",
            "not opted in",
            "--instructions",
            &body_phrase,
        ],
    )?;

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
                input: vec![
                    codex::UserInput::Text {
                        text: "hello".to_string(),
                        text_elements: Vec::new(),
                    },
                    codex::UserInput::Skill {
                        name: "Unscoped".to_string(),
                        path: std::path::PathBuf::from("/gents/skills/unscoped-skill"),
                    },
                ],
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
        captured
            .iter()
            .all(|request| !request.to_string().contains(&body_phrase)),
        "a behavior-scoped skill not in the effective set must not be injected; captured={captured:?}"
    );

    let _ = ws.close(None).await;
    Ok(())
}
