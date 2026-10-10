use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_does_not_clobber_session_agent_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let node_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent_id = format!("{node_did}:default");
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let (mut serve, server_port, _) = spawn_server_with_ready_json_recovering(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    let graphql = graphql_url(server_port);
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{session_id}",
                    node_did: "{node_did}",
                    requester_did: "{node_did}",
                    agent_id: "{default_agent_id}",
                    created_at: "2026-01-01T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
                session_id = escape_graphql_string(&session_id),
                node_did = escape_graphql_string(&node_did),
                default_agent_id = escape_graphql_string(&default_agent_id),
            ),
        ))
        .await?;

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
    let _initialize: codex::InitializeResponse = serve
        .capturing(read_typed_response(&mut ws, request_id(1)))
        .await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(2),
            params: codex::ThreadResumeParams {
                thread_id: session_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let _ = serve.capturing(read_jsonrpc(&mut ws)).await?;

    let resp = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                AgentSession(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        node_did: {{ _eq: "{node_did}" }},
                        requester_did: {{ _eq: "{node_did}" }}
                    }},
                    limit: 1
                ) {{ agent_id }}
            }}"#,
                session_id = escape_graphql_string(&session_id),
                node_did = escape_graphql_string(&node_did),
            ),
        ))
        .await?;
    let preserved_agent_id = resp
        .pointer("/data/AgentSession/0/agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        preserved_agent_id, default_agent_id,
        "agent_id must remain pinned to its create-time value"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_does_not_adopt_a_session_from_another_agent() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let node_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());
    let foreign_agent_id = "some-other-agent".to_string();

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let (mut serve, server_port, _) = spawn_server_with_ready_json_recovering(
        &home_dir,
        server_port,
        &["--codex-shim-port", &shim_port_string],
        &[],
    )?;
    let graphql = graphql_url(server_port);
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{session_id}",
                    node_did: "{node_did}",
                    requester_did: "{node_did}",
                    agent_id: "{foreign_agent_id}",
                    created_at: "2026-01-01T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
                session_id = escape_graphql_string(&session_id),
                node_did = escape_graphql_string(&node_did),
                foreign_agent_id = escape_graphql_string(&foreign_agent_id),
            ),
        ))
        .await?;

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
    let _initialize: codex::InitializeResponse =
        read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(2),
            params: codex::ThreadResumeParams {
                thread_id: session_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(2)).await?;
    assert!(
        error.message.contains("unknown Codex thread"),
        "a session outside the bound agent must not enter the projection: {}",
        error.message
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadArchive {
            request_id: request_id(3),
            params: codex::ThreadArchiveParams {
                thread_id: session_id,
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(3)).await?;
    assert!(
        error.message.contains("unknown Codex thread"),
        "archiving a session outside the bound agent must fail explicitly: {}",
        error.message
    );
    Ok(())
}
