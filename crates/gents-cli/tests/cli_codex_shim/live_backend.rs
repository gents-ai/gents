use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_protocol_uses_real_backend() -> Result<()> {
    let prompt_token = "PONGLIVE";
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;
    let prompt = format!("Reply with exactly this token and no extra words: {prompt_token}");
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let (final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;

    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);
    assert!(
        final_text.contains(prompt_token),
        "expected live Codex protocol stream to contain {prompt_token}, got:\n{final_text}"
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "config/read", "thread/start", "turn/start"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_runtime_spawn_projects_real_subagent() -> Result<()> {
    let suffix = Uuid::new_v4().simple().to_string();
    let child_token = format!("CHILDLIVE-{}", &suffix[..8]);
    let smoke = start_live_codex_shim().await?;
    let child_behavior_id = configure_live_local_subagent(&smoke).await?;
    let expected_child_model = gents_model_selection_id(&smoke.backend_id, &smoke.model_name);

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;
    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;
    let prompt = format!(
        "Call spawn_subagent exactly once using the target named `codex-live-child`, \
         prompt `Reply with exactly {child_token} and no extra words`, and await_mode \
         `foreground`. Do not call any other tool. After the child returns, reply with \
         exactly {child_token} and no extra words."
    );
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let capture = read_turn_capture(&mut ws).await?;

    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&child_token),
        "parent did not return the real child result token {child_token}: {}",
        capture.text
    );
    let (parent_request_id, parent_session_id, parent_behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_eq!(parent_session_id, thread_id);
    assert_eq!(parent_behavior_id, smoke.behavior_id);

    let spawned = wait_for_real_spawn_projection(
        &smoke.graphql,
        &parent_request_id,
        &smoke.agent_did,
        &child_behavior_id,
        &child_token,
    )
    .await?;
    assert_eq!(spawned.parent_session_id, thread_id);

    let completed_spawn = capture
        .completed_collab_items
        .iter()
        .rev()
        .find(|item| {
            item.tool == codex::CollabAgentTool::SpawnAgent
                && item.receiver_thread_ids == vec![spawned.child_session_id.clone()]
                && item.child_status == Some(codex::CollabAgentStatus::Completed)
        })
        .ok_or_else(|| {
            anyhow!(
                "live turn did not project the completed runtime spawn as a native collab item: {:?}",
                capture.completed_collab_items
            )
        })?;
    assert_eq!(
        completed_spawn.status,
        codex::CollabAgentToolCallStatus::Completed
    );
    assert_eq!(
        completed_spawn.model.as_deref(),
        Some(expected_child_model.as_str())
    );

    wait_for_completed_inference_behaviors(
        &smoke.graphql,
        &smoke.backend_id,
        &[&smoke.behavior_id, &child_behavior_id],
    )
    .await?;
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &[
            "initialize",
            "thread/start",
            "turn/start",
            "collabAgentToolCall",
        ],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_gents_filesystem_tools_project_to_codex_items() -> Result<()> {
    let suffix = Uuid::new_v4().simple().to_string();
    let token = format!("FSLIVE-{}", &suffix[..8]);
    let smoke = start_live_codex_shim_with_write_tools(true, None).await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;

    let fixture_dir = smoke.home_dir.join("live-fs-route");
    let fixture_file = fixture_dir.join("fixture.txt");
    let relative_fixture = "live-fs-route/fixture.txt";
    fs::create_dir_all(&fixture_dir)?;
    fs::write(&fixture_file, &token)?;

    let prompt = format!(
        "Use the read_file tool to read `{relative_fixture}` from the current working directory. Reply with exactly the file contents and no extra words."
    );
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let capture = read_turn_capture(&mut ws).await?;

    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&token),
        "expected live backend to read fs route fixture token {token}, got:\n{}",
        capture.text
    );
    assert!(
        capture
            .completed_tools
            .iter()
            .any(|tool| tool.contains("read_file")),
        "live backend did not complete read_file; completed tools: {:?}\ntext:\n{}",
        capture.completed_tools,
        capture.text
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "config/read", "thread/start", "turn/start"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_thread_projection_survives_real_backend_turn() -> Result<()> {
    let prompt_token = "PROJLIVE";
    let thread_name = format!("GENTS live projection {}", Uuid::new_v4().simple());
    let goal_objective = format!("exercise live projection {}", Uuid::new_v4().simple());
    let git_branch = "codex-shim-live-projection".to_string();
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSetName {
            request_id: request_id(401),
            params: codex::ThreadSetNameParams {
                thread_id: thread_id.clone(),
                name: thread_name.clone(),
            },
        },
    )
    .await?;
    let _: codex::ThreadSetNameResponse = read_typed_response(&mut ws, request_id(401)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadMemoryModeSet {
            request_id: request_id(402),
            params: codex::ThreadMemoryModeSetParams {
                thread_id: thread_id.clone(),
                mode: codex::ThreadMemoryMode::Disabled,
            },
        },
    )
    .await?;
    let _: codex::ThreadMemoryModeSetResponse =
        read_typed_response(&mut ws, request_id(402)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadSettingsUpdate {
            request_id: request_id(403),
            params: codex::ThreadSettingsUpdateParams {
                thread_id: thread_id.clone(),
                cwd: Some(smoke.home_dir.clone()),
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::ThreadSettingsUpdateResponse =
        read_typed_response(&mut ws, request_id(403)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalSet {
            request_id: request_id(404),
            params: codex::ThreadGoalSetParams {
                thread_id: thread_id.clone(),
                objective: Some(goal_objective.clone()),
                status: Some(codex::ThreadGoalStatus::Active),
                token_budget: Some(Some(321)),
            },
        },
    )
    .await?;
    let goal_set: codex::ThreadGoalSetResponse =
        read_typed_response(&mut ws, request_id(404)).await?;
    assert_eq!(goal_set.goal.thread_id, thread_id);
    assert_eq!(goal_set.goal.objective, goal_objective);
    assert_eq!(goal_set.goal.token_budget, Some(321));

    let expected_git_sha = init_test_git_repo(&smoke.home_dir, &git_branch)?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadMetadataUpdate {
            request_id: request_id(405),
            params: codex::ThreadMetadataUpdateParams {
                thread_id: thread_id.clone(),
                git_info: Some(codex::ThreadMetadataGitInfoUpdateParams {
                    sha: Some(Some(format!("ignored-{}", Uuid::new_v4().simple()))),
                    branch: Some(Some("ignored-client-branch".to_string())),
                    origin_url: None,
                }),
            },
        },
    )
    .await?;
    let metadata_update: codex::ThreadMetadataUpdateResponse =
        read_typed_response(&mut ws, request_id(405)).await?;
    assert_eq!(
        metadata_update.thread.name.as_deref(),
        Some(thread_name.as_str())
    );
    assert_eq!(
        metadata_update
            .thread
            .git_info
            .as_ref()
            .and_then(|git| git.sha.as_deref()),
        Some(expected_git_sha.as_str())
    );

    let prompt = format!("Reply with exactly this token and no extra words: {prompt_token}");
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let (final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);
    assert!(
        final_text.contains(prompt_token),
        "expected live Codex protocol stream to contain {prompt_token}, got:\n{final_text}"
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);

    let durable_response = graphql_query(
        &smoke.graphql,
        &format!(
            r#"{{
                AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_did
                    behavior_id
                    status
                    started
                }}
                AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_did
                    behavior_id
                    title
                    title_source
                }}
            }}"#,
            escape_graphql_string(&thread_id),
            escape_graphql_string(&thread_id),
        ),
    )
    .await?;
    let session = first_graphql_row(&durable_response, "AgentSession")?;
    assert_eq!(
        session.get("session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        session.get("agent_did").and_then(Value::as_str),
        Some(smoke.agent_did.as_str())
    );
    let expected_behavior_id = format!("{}:default", smoke.agent_did);
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
    let conversation = first_graphql_row(&durable_response, "AgentConversation")?;
    assert_eq!(
        conversation.get("session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        conversation.get("agent_did").and_then(Value::as_str),
        Some(smoke.agent_did.as_str())
    );
    assert_eq!(
        conversation.get("behavior_id").and_then(Value::as_str),
        Some(expected_behavior_id.as_str())
    );
    assert_eq!(
        conversation.get("title").and_then(Value::as_str),
        Some(thread_name.as_str())
    );
    assert_eq!(
        conversation.get("title_source").and_then(Value::as_str),
        Some("user")
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(406),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: false,
            },
        },
    )
    .await?;
    let thread_read: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(406)).await?;
    assert_eq!(thread_read.thread.id, thread_id);
    assert_eq!(
        thread_read.thread.name.as_deref(),
        Some(thread_name.as_str())
    );
    assert_eq!(
        thread_read
            .thread
            .git_info
            .as_ref()
            .and_then(|git| git.branch.as_deref()),
        Some(git_branch.as_str())
    );
    let history_turn = thread_read
        .thread
        .turns
        .iter()
        .find(|turn| turn.id == completed_turn.id)
        .ok_or_else(|| {
            anyhow!(
                "live thread/read did not include turn {}",
                completed_turn.id
            )
        })?;
    assert_turn_has_user_text(history_turn, &prompt);
    assert_turn_has_agent_text(history_turn, prompt_token);

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(407),
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
        read_typed_response(&mut ws, request_id(407)).await?;
    let listed = thread_list
        .data
        .iter()
        .find(|thread| thread.id == thread_id)
        .ok_or_else(|| anyhow!("live GENTS-backed thread list did not include {thread_id}"))?;
    assert_eq!(listed.name.as_deref(), Some(thread_name.as_str()));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalGet {
            request_id: request_id(408),
            params: codex::ThreadGoalGetParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let goal_get: codex::ThreadGoalGetResponse =
        read_typed_response(&mut ws, request_id(408)).await?;
    assert_eq!(
        goal_get.goal.as_ref().map(|goal| goal.objective.as_str()),
        Some(goal_objective.as_str())
    );

    assert_shim_trace_methods(
        &smoke.shim_trace,
        &[
            "initialize",
            "config/read",
            "thread/start",
            "thread/name/set",
            "thread/memoryMode/set",
            "thread/settings/update",
            "thread/goal/set",
            "thread/metadata/update",
            "turn/start",
            "thread/read",
            "thread/list",
            "thread/goal/get",
        ],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_protocol_supports_multiturn_memory() -> Result<()> {
    let memory_token = "LIME7";
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;

    let first_prompt = multiturn_first_prompt(memory_token);
    send_turn(&mut ws, &thread_id, &first_prompt).await?;
    let (_first_text, first_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(first_turn.status, codex::TurnStatus::Completed);
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &first_prompt).await?;
    assert_eq!(session_id, thread_id);

    let second_prompt = "What project codeword did I give earlier in this conversation? Reply with exactly the codeword and no extra words.";
    send_turn(&mut ws, &thread_id, second_prompt).await?;
    let (second_text, second_turn) = read_turn_to_completion(&mut ws).await?;

    assert_eq!(second_turn.status, codex::TurnStatus::Completed);
    assert!(
        second_text.contains(memory_token),
        "expected second live Codex protocol turn to remember {memory_token}, got:\n{second_text}"
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, second_prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "config/read", "thread/start"],
    )?;
    assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/start", 2)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires authenticated gh and the configured real OpenAI-compatible backend"]
async fn codex_shim_live_three_prompt_regression_writes_codex_home_trace() -> Result<()> {
    require_command("gh")?;
    if !gh_is_authenticated() {
        eprintln!("skipping three-prompt live regression: gh is not authenticated");
        return Ok(());
    }
    let repo_root = workspace_root()?;
    let home_root = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let broad_tool_root = home_root
        .as_deref()
        .filter(|home| repo_root.starts_with(home))
        .unwrap_or_else(|| repo_root.parent().unwrap_or(repo_root.as_path()));
    let smoke = start_live_codex_shim_with_write_tools(true, Some(broad_tool_root)).await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &repo_root).await?;

    let cases: &[(&str, &str, &[&str], &str)] = &[
        (
            "repo overview",
            "hey codex! tell mea bout this repo",
            &["gents"],
            "read_file",
        ),
        (
            "github issues and prs",
            "amazing can you use gh to tell me about open issues and prs",
            &["issue", "pr"],
            "gh",
        ),
        (
            "lean state machines",
            "i'd like you to do a deep dive on the lean code and tell me how the state machines defined there interlock and interact",
            &["lean", "state"],
            "read_file",
        ),
    ];
    let mut captures = Vec::new();

    for &(label, prompt, expected_text, expected_tool) in cases {
        send_turn(&mut ws, &thread_id, prompt).await?;
        let capture = read_turn_capture(&mut ws).await?;

        assert_eq!(
            capture.turn.status,
            codex::TurnStatus::Completed,
            "{label} turn did not complete: {:?}",
            capture.turn
        );
        assert_text_contains_all_case_insensitive(&capture.text, label, expected_text);
        assert!(
            capture
                .completed_tools
                .iter()
                .any(|tool| tool.contains(expected_tool)),
            "{label} did not complete expected tool {expected_tool}; completed tools: {:?}\ntext:\n{}",
            capture.completed_tools,
            capture.text
        );
        assert!(
            !capture.started_tools.is_empty(),
            "{label} did not stream any started tool items; events: {:?}\ntext:\n{}",
            capture.event_order,
            capture.text
        );
        assert!(
            turn_had_tool_before_later_agent_text(&capture),
            "{label} did not stream a tool item before later assistant text; events: {:?}\ntext:\n{}",
            capture.event_order,
            capture.text
        );
        assert!(
            !turn_had_tool_after_final_agent_text(&capture),
            "{label} streamed tool items after the final assistant text; events: {:?}\ntext:\n{}",
            capture.event_order,
            capture.text
        );
        assert!(
            capture
                .turn_completed_tool_ids
                .iter()
                .all(|id| capture.completed_tool_ids.contains(id)),
            "{label} turn/completed introduced tool ids that were not streamed first; completed ids: {:?}; turn/completed ids: {:?}",
            capture.completed_tool_ids,
            capture.turn_completed_tool_ids
        );
        assert_eq!(
            capture.turn.items_view,
            codex::TurnItemsView::NotLoaded,
            "{label} turn/completed should not send a replayable full item snapshot"
        );
        assert!(
            capture.turn.items.is_empty(),
            "{label} turn/completed should not repeat streamed items: {:?}",
            capture.turn.items
        );
        let (_request_id, session_id, _behavior_id) =
            wait_for_request(&smoke.graphql, &smoke.agent_did, prompt).await?;
        assert_eq!(session_id, thread_id);
        captures.push(capture);
    }

    let default_trace = smoke.codex_home.join("log").join("codex-shim-events.jsonl");
    assert_eq!(smoke.shim_trace, default_trace);
    assert!(
        smoke.codex_home.is_dir(),
        "expected Codex home to exist at {}",
        smoke.codex_home.display()
    );
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &[
            "initialize",
            "config/read",
            "thread/start",
            "agent_message/delta",
            "item/started",
            "item/completed",
            "turn/completed",
            "mcpToolCall",
            "commandExecution",
            "read_file",
        ],
    )?;
    assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/start", cases.len())?;
    assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/completed", cases.len())?;

    assert_eq!(captures.len(), cases.len());
    Ok(())
}
