use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_protocol_turn_streams_gents_response() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("codex-shim-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

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
    let default_backend_id = default_backend_id(&agent_did);
    let default_model_selection = gents_model_selection_id(&default_backend_id, &model_name);
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
    let initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    assert!(
        initialize.user_agent.starts_with("gents-codex-shim/"),
        "unexpected initialize response: {initialize:?}"
    );

    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(2),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let config: codex::ConfigReadResponse = read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        config.config.model.as_deref(),
        Some(default_model_selection.as_str()),
        "ConfigRead.model should be the bound behavior's backend-qualified model selection"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(3),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(3)).await?;
    let thread_id = thread_start.thread.id.clone();
    Uuid::parse_str(&thread_id)
        .with_context(|| format!("Codex TUI requires UUID thread ids, got {thread_id}"))?;

    let prompt = format!("Reply with exactly {}.", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(4),
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
    let turn_start: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(4)).await?;
    assert_eq!(turn_start.turn.status, codex::TurnStatus::InProgress);

    let turn_capture = read_turn_capture(&mut ws).await?;
    let final_text = turn_capture.text.clone();
    let completed_turn = turn_capture.turn.clone();
    assert_eq!(
        completed_turn.status,
        codex::TurnStatus::Completed,
        "completed_turn={completed_turn:?}; final_text={final_text}"
    );
    assert!(
        final_text.contains(&expected_reply),
        "expected streamed Codex text to contain {expected_reply}, got:\n{final_text}"
    );

    let turn_usage = turn_capture
        .token_usage
        .as_ref()
        .expect("turn completion should emit a ThreadTokenUsageUpdated notification");
    assert!(
        turn_usage.total.total_tokens > 0,
        "expected non-zero cumulative token usage on turn completion, got {turn_usage:?}"
    );
    assert!(
        turn_usage.last.total_tokens > 0,
        "expected non-zero last-turn token usage on turn completion, got {turn_usage:?}"
    );
    assert_eq!(
        turn_usage.model_context_window,
        Some(gents::DEFAULT_CONTEXT_WINDOW as i64),
        "context capacity should come from the bound GENTS inference profile"
    );

    let session_response = serve
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{
                AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_did
                    behavior_id
                    status
                    started
                }}
            }}"#,
                escape_graphql_string(&thread_id),
            ),
        ))
        .await?;
    let session = first_graphql_row(&session_response, "AgentSession")?;
    assert_eq!(
        session.get("session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        session.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    let expected_behavior_id = format!("{agent_did}:default");
    assert_eq!(
        session.get("behavior_id").and_then(Value::as_str),
        Some(expected_behavior_id.as_str())
    );
    assert_eq!(
        session.get("status").and_then(Value::as_str),
        Some("active")
    );
    assert!(
        session
            .get("started")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty()),
        "AgentSession.started should be populated: {session}"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(30),
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
    let thread_list: codex::ThreadListResponse =
        read_typed_response(&mut ws, request_id(30)).await?;
    assert!(
        thread_list.data.iter().any(|thread| thread.id == thread_id),
        "GENTS-backed thread list did not include {thread_id}: {thread_list:?}"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadLoadedList {
            request_id: request_id(31),
            params: codex::ThreadLoadedListParams::default(),
        },
    )
    .await?;
    let loaded_threads: codex::ThreadLoadedListResponse =
        read_typed_response(&mut ws, request_id(31)).await?;
    assert!(loaded_threads.data.contains(&thread_id));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(32),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let thread_read: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(32)).await?;
    assert_eq!(thread_read.thread.id, thread_id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(33),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_resume: codex::ThreadResumeResponse = read_typed_response(&mut ws, request_id(33))
        .await
        .context("reading thread/resume response")?;
    assert_eq!(thread_resume.thread.id, thread_id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSetName {
            request_id: request_id(34),
            params: codex::ThreadSetNameParams {
                thread_id: thread_id.clone(),
                name: "GENTS-backed Codex thread".to_string(),
            },
        },
    )
    .await?;
    let _: codex::ThreadSetNameResponse = read_typed_response(&mut ws, request_id(34))
        .await
        .context("reading thread/name/set response")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadMemoryModeSet {
            request_id: request_id(35),
            params: codex::ThreadMemoryModeSetParams {
                thread_id: thread_id.clone(),
                mode: codex::ThreadMemoryMode::Disabled,
            },
        },
    )
    .await?;
    let _: codex::ThreadMemoryModeSetResponse = read_typed_response(&mut ws, request_id(35))
        .await
        .context("reading thread/memoryMode/set response")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSettingsUpdate {
            request_id: request_id(36),
            params: codex::ThreadSettingsUpdateParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.clone()),
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::ThreadSettingsUpdateResponse = read_typed_response(&mut ws, request_id(36))
        .await
        .context("reading thread/settings/update response")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalSet {
            request_id: request_id(37),
            params: codex::ThreadGoalSetParams {
                thread_id: thread_id.clone(),
                objective: Some("exercise GENTS-backed Codex goal state".to_string()),
                status: Some(codex::ThreadGoalStatus::Active),
                token_budget: Some(Some(123)),
            },
        },
    )
    .await?;
    let goal_set: codex::ThreadGoalSetResponse = read_typed_response(&mut ws, request_id(37))
        .await
        .context("reading thread/goal/set response")?;
    assert_eq!(goal_set.goal.thread_id, thread_id);
    assert_eq!(
        goal_set.goal.objective,
        "exercise GENTS-backed Codex goal state"
    );
    assert_eq!(goal_set.goal.token_budget, Some(123));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalGet {
            request_id: request_id(38),
            params: codex::ThreadGoalGetParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let goal_get: codex::ThreadGoalGetResponse = read_typed_response(&mut ws, request_id(38))
        .await
        .context("reading thread/goal/get response")?;
    assert_eq!(
        goal_get.goal.as_ref().map(|goal| &goal.thread_id),
        Some(&thread_id)
    );

    let expected_git_sha = init_test_git_repo(&home_dir, "main")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadMetadataUpdate {
            request_id: request_id(39),
            params: codex::ThreadMetadataUpdateParams {
                thread_id: thread_id.clone(),
                git_info: Some(codex::ThreadMetadataGitInfoUpdateParams {
                    sha: Some(Some("abc123".to_string())),
                    branch: Some(Some("main".to_string())),
                    origin_url: None,
                }),
            },
        },
    )
    .await?;
    let metadata_update: codex::ThreadMetadataUpdateResponse =
        read_typed_response(&mut ws, request_id(39))
            .await
            .context("reading thread/metadata/update response")?;
    assert_eq!(
        metadata_update
            .thread
            .git_info
            .as_ref()
            .and_then(|git| git.sha.as_deref()),
        Some(expected_git_sha.as_str())
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadArchive {
            request_id: request_id(40),
            params: codex::ThreadArchiveParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let _: codex::ThreadArchiveResponse = read_typed_response(&mut ws, request_id(40))
        .await
        .context("reading thread/archive response")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(48),
            params: codex::ThreadListParams {
                cursor: None,
                limit: Some(1),
                sort_key: None,
                sort_direction: None,
                model_providers: Some(vec!["gents".to_string()]),
                source_kinds: Some(vec![codex::ThreadSourceKind::Cli]),
                archived: Some(true),
                cwd: Some(codex::ThreadListCwdFilter::One(
                    home_dir.display().to_string(),
                )),
                use_state_db_only: true,
                search_term: Some("GENTS-backed Codex thread".to_string()),
            },
        },
    )
    .await?;
    let archived_threads: codex::ThreadListResponse = read_typed_response(&mut ws, request_id(48))
        .await
        .context("reading archived thread/list response")?;
    assert_eq!(archived_threads.data.len(), 1);
    assert_eq!(archived_threads.data[0].id, thread_id);
    assert_eq!(
        archived_threads.backwards_cursor.as_deref(),
        Some(thread_id.as_str())
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(49),
            params: codex::ThreadListParams {
                cursor: None,
                limit: None,
                sort_key: None,
                sort_direction: None,
                model_providers: Some(vec!["openai".to_string()]),
                source_kinds: Some(vec![codex::ThreadSourceKind::Cli]),
                archived: Some(true),
                cwd: None,
                use_state_db_only: true,
                search_term: None,
            },
        },
    )
    .await?;
    let wrong_provider_threads: codex::ThreadListResponse =
        read_typed_response(&mut ws, request_id(49))
            .await
            .context("reading provider-filtered thread/list response")?;
    assert!(wrong_provider_threads.data.is_empty());

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadUnarchive {
            request_id: request_id(41),
            params: codex::ThreadUnarchiveParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let thread_unarchive: codex::ThreadUnarchiveResponse =
        read_typed_response(&mut ws, request_id(41))
            .await
            .context("reading thread/unarchive response")?;
    assert_eq!(thread_unarchive.thread.id, thread_id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalGet {
            request_id: request_id(50),
            params: codex::ThreadGoalGetParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let goal_after_turn: codex::ThreadGoalGetResponse =
        read_typed_response(&mut ws, request_id(50))
            .await
            .context("reading post-turn thread/goal/get response")?;
    let goal_after_turn = goal_after_turn
        .goal
        .expect("goal should still exist after the turn");
    assert!(
        goal_after_turn.tokens_used > 0,
        "goal.tokens_used should reflect real session usage after a turn, got {}",
        goal_after_turn.tokens_used
    );

    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &prompt)),
        "mock endpoint did not receive the Codex prompt; captured={captured_requests:?}"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(42),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let thread_history: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(42)).await?;
    assert_eq!(thread_history.thread.id, thread_id);
    assert_eq!(thread_history.thread.turns.len(), 1);
    let history_turn = &thread_history.thread.turns[0];
    assert_eq!(history_turn.id, completed_turn.id);
    assert_eq!(history_turn.items_view, codex::TurnItemsView::Full);
    assert_eq!(history_turn.status, codex::TurnStatus::Completed);
    assert_turn_has_user_text(history_turn, &prompt);
    assert_turn_has_agent_text(history_turn, &expected_reply);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(46),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let resumed_history: codex::ThreadResumeResponse = read_typed_response(&mut ws, request_id(46))
        .await
        .context("reading history-bearing thread/resume response")?;
    assert_eq!(resumed_history.thread.id, thread_id);
    assert_eq!(resumed_history.thread.turns.len(), 1);
    let resumed_turn = &resumed_history.thread.turns[0];
    assert_eq!(resumed_turn.id, completed_turn.id);
    assert_eq!(resumed_turn.items_view, codex::TurnItemsView::Full);
    assert_eq!(resumed_turn.status, codex::TurnStatus::Completed);
    assert_turn_has_user_text(resumed_turn, &prompt);
    assert_turn_has_agent_text(resumed_turn, &expected_reply);

    let replay_usage = read_token_usage_notification(&mut ws)
        .await
        .context("reading token-usage replay after thread/resume")?;
    assert!(
        replay_usage.total.total_tokens > 0,
        "thread/resume should replay non-zero session token usage, got {replay_usage:?}"
    );
    assert_eq!(
        replay_usage.last.total_tokens, turn_usage.last.total_tokens,
        "thread/resume should restore the latest inference context, not cumulative usage"
    );
    assert_eq!(
        replay_usage.model_context_window, turn_usage.model_context_window,
        "thread/resume should restore the effective context capacity"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(47),
            params: codex::ThreadResumeParams {
                thread_id: thread_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                exclude_turns: true,
                ..Default::default()
            },
        },
    )
    .await?;
    let metadata_resume: codex::ThreadResumeResponse = read_typed_response(&mut ws, request_id(47))
        .await
        .context("reading metadata-only thread/resume response")?;
    assert_eq!(metadata_resume.thread.id, thread_id);
    assert!(
        metadata_resume.thread.turns.is_empty(),
        "excludeTurns resume should not load persisted turns"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadTurnsList {
            request_id: request_id(43),
            params: codex::ThreadTurnsListParams {
                thread_id: thread_id.clone(),
                cursor: None,
                limit: None,
                sort_direction: None,
                items_view: None,
            },
        },
    )
    .await?;
    let turns_list: codex::ThreadTurnsListResponse =
        read_typed_response(&mut ws, request_id(43)).await?;
    assert_eq!(turns_list.data.len(), 1);
    assert_eq!(turns_list.data[0].id, completed_turn.id);
    assert_eq!(turns_list.data[0].items_view, codex::TurnItemsView::Summary);
    assert_turn_has_user_text(&turns_list.data[0], &prompt);
    assert_turn_has_agent_text(&turns_list.data[0], &expected_reply);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadTurnsItemsList {
            request_id: request_id(44),
            params: codex::ThreadTurnsItemsListParams {
                thread_id: thread_id.clone(),
                turn_id: completed_turn.id.clone(),
                cursor: None,
                limit: None,
                sort_direction: None,
            },
        },
    )
    .await?;
    let items_list: codex::ThreadTurnsItemsListResponse =
        read_typed_response(&mut ws, request_id(44)).await?;
    assert!(
        items_list.data.len() >= 2,
        "expected persisted turn items, got {:?}",
        items_list.data
    );

    send_raw_client_request(
        &mut ws,
        request_id(45),
        "getConversationSummary",
        json!({ "conversationId": thread_id.clone() }),
    )
    .await?;
    let summary: codex::GetConversationSummaryResponse =
        read_typed_response(&mut ws, request_id(45)).await?;
    assert_eq!(summary.summary.conversation_id.to_string(), thread_id);
    assert_eq!(summary.summary.model_provider, "gents");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_completes_blank_materialized_terminal_message() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-blank-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-blank-{}", Uuid::new_v4().simple());
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
            "50",
            "--codex-shim-timeout-secs",
            "5",
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

    let prompt = "Read notes.txt, then finish without visible final text.";
    send_turn(&mut ws, &thread_id, prompt).await?;
    let (request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &agent_did, prompt).await?;
    assert_eq!(session_id, thread_id);
    seed_blank_materialized_completion(&graphql, &request_id, &agent_did, &behavior_id, &thread_id)
        .await?;

    let capture = tokio::time::timeout(Duration::from_secs(15), read_turn_capture(&mut ws))
        .await
        .context("timed out waiting for Codex shim turn completion")??;

    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.trim().is_empty(),
        "mock final response is intentionally blank; got:\n{}",
        capture.text
    );

    Ok(())
}
