use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_derives_git_info_and_keeps_empty_thread_ephemeral() -> Result<()> {
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

    let git_dir = tempdir.path().join("repo");
    fs::create_dir_all(&git_dir)?;
    let expected_sha = init_test_git_repo(&git_dir, "main")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(2),
            params: codex::ThreadStartParams {
                cwd: Some(git_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let git_thread: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        git_thread
            .thread
            .git_info
            .as_ref()
            .and_then(|git| git.sha.as_deref()),
        Some(expected_sha.as_str()),
        "git sha should be derived from the thread cwd at ThreadStart: {:?}",
        git_thread.thread.git_info
    );
    assert_eq!(
        git_thread
            .thread
            .git_info
            .as_ref()
            .and_then(|git| git.branch.as_deref()),
        Some("main"),
        "git branch should be derived from the thread cwd"
    );

    let git_thread_id = git_thread.thread.id.clone();
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSetName {
            request_id: request_id(3),
            params: codex::ThreadSetNameParams {
                thread_id: git_thread_id.clone(),
                name: "Named before first turn".to_string(),
            },
        },
    )
    .await?;
    let _: codex::ThreadSetNameResponse = read_typed_response(&mut ws, request_id(3))
        .await
        .context("reading early thread/name/set response")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(4),
            params: codex::ThreadReadParams {
                thread_id: git_thread_id.clone(),
                include_turns: false,
            },
        },
    )
    .await?;
    let renamed: codex::ThreadReadResponse = read_typed_response(&mut ws, request_id(4))
        .await
        .context("reading thread/read after early rename")?;
    assert_eq!(
        renamed.thread.name.as_deref(),
        Some("Named before first turn"),
        "the adapter should retain an empty thread's presentation state"
    );
    let empty_projection = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                    AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }}
                    AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }}
                }}"#,
                escape_graphql_string(&git_thread_id),
                escape_graphql_string(&git_thread_id),
            ),
        ))
        .await?;
    assert_eq!(empty_projection.pointer("/data/AgentSession/0"), None);
    assert_eq!(empty_projection.pointer("/data/AgentConversation/0"), None);

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(4),
            params: codex::TurnStartParams {
                thread_id: git_thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "materialize this session".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(4)).await?;
    let _ = read_turn_capture(&mut ws).await?;
    let canonical = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{ AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ title }} }}"#,
                escape_graphql_string(&git_thread_id),
            ),
        ))
        .await?;
    assert_eq!(
        canonical
            .pointer("/data/AgentConversation/0/title")
            .and_then(Value::as_str),
        Some("Named before first turn")
    );

    let plain_dir = tempdir.path().join("plain");
    fs::create_dir_all(&plain_dir)?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(5),
            params: codex::ThreadStartParams {
                cwd: Some(plain_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let plain_thread: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(5)).await?;
    assert!(
        plain_thread.thread.git_info.is_none(),
        "non-git cwd should yield no gitInfo, got {:?}",
        plain_thread.thread.git_info
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_thread_fork_and_search_project_gents_sessions() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("fork-search-reply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-fork-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-fork-{}", Uuid::new_v4().simple());
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

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let search_token = format!("FORKSEARCH{}", Uuid::new_v4().simple());
    let prompt = format!("Reply with exactly {search_token} and no extra words.");
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let (_final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadFork {
            request_id: request_id(120),
            params: codex::ThreadForkParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let forked: codex::ThreadForkResponse = read_typed_response(&mut ws, request_id(120)).await?;
    let forked_id = forked.thread.id.clone();
    assert_ne!(forked_id, thread_id);
    assert_eq!(forked.thread.session_id, forked_id);
    assert_eq!(
        forked.thread.forked_from_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert_eq!(forked.thread.status, codex::ThreadStatus::Idle);
    assert_eq!(forked.thread.turns.len(), 1);
    assert_turn_has_user_text(&forked.thread.turns[0], &prompt);
    assert_turn_has_agent_text(&forked.thread.turns[0], &expected_reply);

    let forked_conversation = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    forked_from_session_id
                    fork_at_user_turn
                }}
            }}"#,
                escape_graphql_string(&forked_id),
            ),
        ))
        .await?;
    let child = first_graphql_row(&forked_conversation, "AgentConversation")?;
    assert_eq!(
        child.get("forked_from_session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        child.get("fork_at_user_turn").and_then(Value::as_i64),
        Some(1)
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(121),
            params: codex::ThreadReadParams {
                thread_id: forked_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let forked_read: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(121)).await?;
    assert_eq!(forked_read.thread.id, forked_id);
    assert_eq!(
        forked_read.thread.forked_from_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert_eq!(forked_read.thread.turns.len(), 1);
    assert_turn_has_user_text(&forked_read.thread.turns[0], &prompt);
    assert_turn_has_agent_text(&forked_read.thread.turns[0], &expected_reply);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSearch {
            request_id: request_id(122),
            params: codex::ThreadSearchParams {
                cursor: None,
                limit: None,
                sort_key: None,
                sort_direction: None,
                source_kinds: None,
                archived: None,
                search_term: search_token.clone(),
            },
        },
    )
    .await?;
    let search: codex::ThreadSearchResponse = read_typed_response(&mut ws, request_id(122)).await?;
    let result_ids = search
        .data
        .iter()
        .map(|result| result.thread.id.as_str())
        .collect::<Vec<_>>();
    assert!(
        result_ids.contains(&thread_id.as_str()),
        "thread/search did not include source thread {thread_id}: {search:?}"
    );
    assert!(
        result_ids.contains(&forked_id.as_str()),
        "thread/search did not include forked thread {forked_id}: {search:?}"
    );
    assert!(
        search
            .data
            .iter()
            .any(|result| result.snippet.contains(&search_token)),
        "thread/search snippets did not include token {search_token}: {search:?}"
    );

    Ok(())
}
