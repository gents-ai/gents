use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_thread_list_reconstructs_turned_threads_from_durable_data() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "unused")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());
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

    let turned_session_id = Uuid::new_v4().to_string();
    for mutation in [
        format!(
            r#"mutation {{ create_AgentSession(input: {{
                session_id: "{s}", agent_name: "{behavior_id}", agent_did: "{agent_did}",
                behavior_id: "{behavior_id}", started: "2026-01-01T00:00:00Z", status: "active"
            }}) {{ _docID }} }}"#,
            s = escape_graphql_string(&turned_session_id),
            behavior_id = escape_graphql_string(&behavior_id),
            agent_did = escape_graphql_string(&agent_did),
        ),
        format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{r}", agent_did: "{agent_did}", behavior_id: "{behavior_id}",
                session_id: "{s}", metadata: "{{\"codex_shim\":{{}}}}",
                execution_origin: "interactive", created_at: "2026-01-01T00:00:00Z"
            }}) {{ _docID }} }}"#,
            r = escape_graphql_string(&Uuid::new_v4().to_string()),
            agent_did = escape_graphql_string(&agent_did),
            behavior_id = escape_graphql_string(&behavior_id),
            s = escape_graphql_string(&turned_session_id),
        ),
        format!(
            r#"mutation {{ create_AgentConversation(input: {{
                session_id: "{s}", agent_name: "{behavior_id}", agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                title: "Earlier Codex thread", title_source: "user",
                status: "active", created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z"
            }}) {{ _docID }} }}"#,
            s = escape_graphql_string(&turned_session_id),
            behavior_id = escape_graphql_string(&behavior_id),
            agent_did = escape_graphql_string(&agent_did),
        ),
    ] {
        serve.capturing(graphql_query(&graphql, &mutation)).await?;
    }

    let zero_turn_session_id = Uuid::new_v4().to_string();
    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{ create_AgentSession(input: {{
                session_id: "{s}", agent_name: "{behavior_id}", agent_did: "{agent_did}",
                behavior_id: "{behavior_id}", started: "2026-01-01T00:00:00Z", status: "active"
            }}) {{ _docID }} }}"#,
                s = escape_graphql_string(&zero_turn_session_id),
                behavior_id = escape_graphql_string(&behavior_id),
                agent_did = escape_graphql_string(&agent_did),
            ),
        ))
        .await?;

    let pending_session_id = Uuid::new_v4().to_string();
    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{ create_AgentRequest(input: {{
                    request_id: "{request}", agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}", session_id: "{session}",
                    content: "pending projection", status: "pending",
                    lifecycle_state: "pending", execution_origin: "interactive",
                    metadata: "{{\"codex_shim\":{{}}}}",
                    created_at: "2026-01-01T00:00:01Z"
                }}) {{ _docID }} }}"#,
                request = escape_graphql_string(&Uuid::new_v4().to_string()),
                agent_did = escape_graphql_string(&agent_did),
                behavior_id = escape_graphql_string(&behavior_id),
                session = escape_graphql_string(&pending_session_id),
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
    let _: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(2),
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
    let listed: codex::ThreadListResponse = read_typed_response(&mut ws, request_id(2)).await?;
    assert!(
        listed
            .data
            .iter()
            .any(|thread| thread.id == turned_session_id),
        "a turned Codex thread must be reconstructed from its durable codex_shim request, \
         with no in-process marker: {listed:?}"
    );
    assert!(
        !listed
            .data
            .iter()
            .any(|thread| thread.id == zero_turn_session_id),
        "a never-turned session holds no durable Codex data and must not be surfaced \
         after restart: {listed:?}"
    );
    assert!(
        !listed
            .data
            .iter()
            .any(|thread| thread.id == pending_session_id),
        "a request alone must not invent a thread before the authoritative conversation projection exists: {listed:?}"
    );

    let turned = listed
        .data
        .iter()
        .find(|thread| thread.id == turned_session_id)
        .expect("turned thread present");
    assert_eq!(
        turned.name.as_deref(),
        Some("Earlier Codex thread"),
        "reconstructed thread name should come from the durable conversation title"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_thread_list_projects_canonical_gents_sessions() -> Result<()> {
    // Codex is a view over canonical Gents sessions. The source of the request
    // does not create a second class of persisted conversation.
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "unused")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());
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

    let foreign_session_id = Uuid::new_v4().to_string();
    let foreign_request_id = Uuid::new_v4().to_string();
    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{session}",
                    agent_name: "{agent_name}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    started: "2026-01-01T00:00:00Z",
                    status: "active"
                }}) {{ _docID }}
            }}"#,
                session = escape_graphql_string(&foreign_session_id),
                agent_name = escape_graphql_string(&agent_name),
                agent_did = escape_graphql_string(&agent_did),
                behavior_id = escape_graphql_string(&behavior_id),
            ),
        ))
        .await?;
    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session}",
                    metadata: "{{}}",
                    execution_origin: "cli",
                    created_at: "2026-01-01T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
                request = escape_graphql_string(&foreign_request_id),
                agent_did = escape_graphql_string(&agent_did),
                behavior_id = escape_graphql_string(&behavior_id),
                session = escape_graphql_string(&foreign_session_id),
            ),
        ))
        .await?;
    serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_AgentConversation(input: {{
                    session_id: "{session}",
                    agent_name: "{agent_name}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    title: "Shared Gents session",
                    title_source: "user",
                    preview_text: "shared",
                    status: "active",
                    created_at: "2026-01-01T00:00:00Z",
                    updated_at: "2026-01-01T00:00:00Z",
                    latest_request_id: "{request}"
                }}) {{ _docID }}
            }}"#,
                session = escape_graphql_string(&foreign_session_id),
                request = escape_graphql_string(&foreign_request_id),
                agent_name = escape_graphql_string(&agent_name),
                agent_did = escape_graphql_string(&agent_did),
                behavior_id = escape_graphql_string(&behavior_id),
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
    let _: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    // An empty shim-created thread is visible from the process-local adapter.
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
    let codex_thread: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(2)).await?;
    let codex_thread_id = codex_thread.thread.id.clone();

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(3),
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
    let listed: codex::ThreadListResponse = read_typed_response(&mut ws, request_id(3)).await?;
    assert!(
        listed
            .data
            .iter()
            .any(|thread| thread.id == codex_thread_id),
        "the shim-created Codex thread should be listed: {listed:?}"
    );
    assert!(
        listed
            .data
            .iter()
            .any(|thread| thread.id == foreign_session_id),
        "a canonical Gents session must be visible through the Codex projection: {listed:?}"
    );

    Ok(())
}
