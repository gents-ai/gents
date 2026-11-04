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

fn agent_route(agent: &str) -> String {
    format!("/agents/{agent}")
}

/// One finished turn, so the thread is durable as well as in the sidecar.
async fn thread_with_a_turn(ws: &mut ShimWebSocket, home_dir: &std::path::Path) -> Result<String> {
    let thread = start_thread(ws, home_dir).await?;
    send_turn(ws, &thread, &format!("hello {}", Uuid::new_v4())).await?;
    read_turn_to_completion(ws).await?;
    Ok(thread)
}

async fn assert_sees(ws: &mut ShimWebSocket, thread: &str, sees: bool) -> Result<()> {
    let listed = listed_thread_ids(ws).await?;
    let loaded = loaded_thread_ids(ws).await?;
    assert_eq!(listed.iter().any(|id| id == thread), sees, "{listed:?}");
    assert_eq!(loaded.iter().any(|id| id == thread), sees, "{loaded:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_agents_side_by_side_do_not_see_each_others_threads() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    let other = shim.add_agent("other").await?;
    let mut a = shim.connect(&agent_route(&shim.default_agent)).await?;
    let mut b = shim.connect(&agent_route(&other)).await?;

    let a_thread = thread_with_a_turn(&mut a, &shim.home_dir).await?;
    assert_sees(&mut b, &a_thread, false).await?;

    let b_thread = thread_with_a_turn(&mut b, &shim.home_dir).await?;
    assert_sees(&mut a, &b_thread, false).await?;
    assert_sees(&mut a, &a_thread, true).await?;
    assert_sees(&mut b, &b_thread, true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_windows_on_one_agent_share_its_threads() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    let other = shim.add_agent("other").await?;
    let route = agent_route(&other);
    let mut first = shim.connect(&route).await?;
    let mut second = shim.connect(&route).await?;

    let thread = start_thread(&mut first, &shim.home_dir).await?;

    assert_sees(&mut second, &thread, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_percent_encoded_did_agent_id_connects() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    shim.wait_until_runnable(&shim.default_agent, None).await?;
    let encoded = shim.default_agent.replace(':', "%3A");
    assert_ne!(encoded, shim.default_agent);

    let mut ws = shim.connect(&agent_route(&encoded)).await?;
    let thread = thread_with_a_turn(&mut ws, &shim.home_dir).await?;

    assert_sees(&mut ws, &thread, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_agent_route_shares_the_root_routes_sidecar() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    shim.wait_until_runnable(&shim.default_agent, None).await?;
    let mut root = shim.connect("/").await?;
    let mut routed = shim.connect(&agent_route(&shim.default_agent)).await?;

    let from_root = start_thread(&mut root, &shim.home_dir).await?;
    assert_sees(&mut routed, &from_root, true).await?;

    let from_route = start_thread(&mut routed, &shim.home_dir).await?;
    assert_sees(&mut root, &from_route, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_agents_get_404() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    shim.wait_until_runnable(&shim.default_agent, None).await?;

    let refused = connect_async(shim.url(&agent_route("nosuch")))
        .await
        .expect_err("an agent outside the published set is refused");

    super::server_lifecycle::assert_http_status(refused, StatusCode::NOT_FOUND)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_routes_need_the_same_bearer_token() -> Result<()> {
    let shim = AgentShim::start(
        |_| {
            vec![
                "--codex-shim-auth-token-env".to_string(),
                "GENTS_SHIM_TEST_TOKEN".to_string(),
            ]
        },
        &[("GENTS_SHIM_TEST_TOKEN", "correct-secret")],
    )
    .await?;
    shim.wait_until_runnable(&shim.default_agent, Some("correct-secret"))
        .await?;
    let url = shim.url(&agent_route(&shim.default_agent));

    let refused = connect_async(url.as_str())
        .await
        .expect_err("a missing token is refused");
    super::server_lifecycle::assert_http_status(refused, StatusCode::UNAUTHORIZED)?;

    let refused = connect_async(shim.url(&agent_route("nosuch")))
        .await
        .expect_err("a missing token learns nothing about which agents exist");
    super::server_lifecycle::assert_http_status(refused, StatusCode::UNAUTHORIZED)?;

    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer correct-secret"),
    );
    connect_async(request).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_agent_route_with_a_pinned_agent() -> Result<()> {
    let shim = AgentShim::start(
        |default_agent| {
            vec![
                "--codex-shim-agent-id".to_string(),
                default_agent.to_string(),
            ]
        },
        &[],
    )
    .await?;

    let refused = connect_async(shim.url(&agent_route(&shim.default_agent)))
        .await
        .expect_err("a pinned shim serves one agent");

    super::server_lifecycle::assert_http_status(refused, StatusCode::NOT_FOUND)?;
    assert_eq!(
        shim.runtime(None).await?.get("runnable_agents"),
        Some(&Value::Null)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_agent_route_with_a_public_url() -> Result<()> {
    let shim = AgentShim::start(
        |_| {
            vec![
                "--codex-shim-auth-token-env".to_string(),
                "GENTS_SHIM_TEST_TOKEN".to_string(),
                "--codex-shim-public-url".to_string(),
                "wss://shim.example:8443/".to_string(),
            ]
        },
        &[("GENTS_SHIM_TEST_TOKEN", "correct-secret")],
    )
    .await?;
    let mut request = shim
        .url(&agent_route(&shim.default_agent))
        .into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer correct-secret"),
    );

    let refused = connect_async(request)
        .await
        .expect_err("a token for a published endpoint reaches one agent");

    super::server_lifecycle::assert_http_status(refused, StatusCode::NOT_FOUND)?;
    assert_eq!(
        shim.runtime(Some("correct-secret"))
            .await?
            .get("runnable_agents"),
        Some(&Value::Null)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_root_route_is_unchanged() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    shim.add_agent("other").await?;
    let mut root = shim.connect("/").await?;

    let thread = start_thread(&mut root, &shim.home_dir).await?;
    let prompt = format!("hello {}", Uuid::new_v4());
    send_turn(&mut root, &thread, &prompt).await?;
    read_turn_to_completion(&mut root).await?;

    let (_, session, agent) = wait_for_request(&shim.graphql, &shim.node_did, &prompt).await?;
    assert_eq!(session, thread);
    assert_eq!(agent, shim.default_agent);
    assert_sees(&mut root, &thread, true).await
}
