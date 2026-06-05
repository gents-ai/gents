mod support;
use support::*;

use std::fs;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use codex_app_server_protocol as codex;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type ShimWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const LIVE_CODEX_SHIM_TIMEOUT_SECS: &str = "900";

fn defra_model_selection_id(backend_id: &str, model_name: &str) -> String {
    format!("{backend_id}::{model_name}")
}

fn default_backend_id(agent_did: &str) -> String {
    format!("{agent_did}:backend")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_protocol_turn_streams_defra_response() -> Result<()> {
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_backend_id = default_backend_id(&agent_did);
    let default_model_selection = defra_model_selection_id(&default_backend_id, &model_name);
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        initialize.user_agent.starts_with("defra-agent-codex-shim/"),
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

    let projection_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                CodexThreadProjection(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    cwd
                    archived
                    loaded
                }}
                AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    behavior_id
                    status
                }}
            }}"#,
            escape_graphql_string(&thread_id),
            escape_graphql_string(&thread_id),
        ),
    )
    .await?;
    let projection = first_graphql_row(&projection_response, "CodexThreadProjection")?;
    assert_eq!(
        projection.get("session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    let expected_cwd = home_dir.display().to_string();
    assert_eq!(
        projection.get("cwd").and_then(Value::as_str),
        Some(expected_cwd.as_str())
    );
    assert_eq!(
        projection.get("archived").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        projection.get("loaded").and_then(Value::as_bool),
        Some(true)
    );
    let session = first_graphql_row(&projection_response, "AgentSession")?;
    let expected_behavior_id = format!("{agent_did}:default");
    assert_eq!(
        session.get("behavior_id").and_then(Value::as_str),
        Some(expected_behavior_id.as_str())
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
        "DEFRA-backed thread list did not include {thread_id}: {thread_list:?}"
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
                name: "DEFRA-backed Codex thread".to_string(),
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
                objective: Some("exercise DEFRA-backed Codex goal state".to_string()),
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
        "exercise DEFRA-backed Codex goal state"
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
        Some("abc123")
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
                model_providers: Some(vec!["defra".to_string()]),
                source_kinds: Some(vec![codex::ThreadSourceKind::Cli]),
                archived: Some(true),
                cwd: Some(codex::ThreadListCwdFilter::One(
                    home_dir.display().to_string(),
                )),
                use_state_db_only: true,
                search_term: Some("DEFRA-backed Codex thread".to_string()),
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

    let (final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);
    assert!(
        final_text.contains(&expected_reply),
        "expected streamed Codex text to contain {expected_reply}, got:\n{final_text}"
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
    assert_eq!(summary.summary.model_provider, "defra");

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
            "5",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_thread_fork_and_search_project_defra_sessions() -> Result<()> {
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
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

    let forked_conversation = graphql_query(
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
    )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_fs_routes_are_unsupported() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("unused-fs-unsupported-reply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-fs-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-fs-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;

    for (idx, method, params) in [
        (
            0,
            "fs/readFile",
            json!({ "path": home_dir.join("file.txt").display().to_string() }),
        ),
        (
            1,
            "fs/writeFile",
            json!({
                "path": home_dir.join("file.txt").display().to_string(),
                "dataBase64": "ZGVmcmE=",
            }),
        ),
        (
            2,
            "fs/createDirectory",
            json!({
                "path": home_dir.join("dir").display().to_string(),
                "recursive": true,
            }),
        ),
        (
            3,
            "fs/getMetadata",
            json!({ "path": home_dir.display().to_string() }),
        ),
        (
            4,
            "fs/readDirectory",
            json!({ "path": home_dir.display().to_string() }),
        ),
        (
            5,
            "fs/remove",
            json!({
                "path": home_dir.join("file.txt").display().to_string(),
                "recursive": true,
                "force": true,
            }),
        ),
        (
            6,
            "fs/copy",
            json!({
                "sourcePath": home_dir.join("file.txt").display().to_string(),
                "destinationPath": home_dir.join("copy.txt").display().to_string(),
                "recursive": false,
            }),
        ),
        (
            7,
            "fs/watch",
            json!({
                "watchId": "watch-unsupported",
                "path": home_dir.display().to_string(),
            }),
        ),
        (8, "fs/unwatch", json!({ "watchId": "watch-unsupported" })),
    ] {
        let id = request_id(501 + idx);
        send_raw_client_request(&mut ws, id.clone(), method, params).await?;
        let error = read_error_response(&mut ws, id).await?;
        assert_eq!(error.code, -32601);
        assert!(
            error.message.contains("unsupported Codex shim method"),
            "unexpected fs/* unsupported message for {method}: {error:?}"
        );
        assert!(
            error
                .message
                .contains("model filesystem activity must run through DEFRA"),
            "fs/* error should describe the DEFRA tool-call boundary for {method}: {error:?}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_host_runtime_routes_cover_low_risk_paths() -> Result<()> {
    require_command("git")?;
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("unused-host-runtime-reply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-host-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-host-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;

    send_raw_client_request(
        &mut ws,
        request_id(551),
        "command/exec",
        json!({
            "command": ["/bin/sh", "-lc", "printf defra-host-exec"],
            "cwd": home_dir.display().to_string(),
            "timeoutMs": 5000,
        }),
    )
    .await?;
    let exec_error = read_error_response(&mut ws, request_id(551)).await?;
    assert_eq!(exec_error.code, -32601);
    assert!(exec_error.message.contains("DEFRA tool-call"));

    send_raw_client_request(
        &mut ws,
        request_id(581),
        "process/spawn",
        json!({
            "command": ["/bin/sh", "-lc", "printf defra-process-spawn"],
            "processHandle": format!("process-{}", Uuid::new_v4().simple()),
            "cwd": home_dir.display().to_string(),
            "streamStdoutStderr": true,
            "timeoutMs": 5000,
        }),
    )
    .await?;
    let process_error = read_error_response(&mut ws, request_id(581)).await?;
    assert_eq!(process_error.code, -32601);
    assert!(process_error
        .message
        .contains("managed-exec state machines"));

    fs::write(home_dir.join("alpha_notes.txt"), "alpha")?;
    fs::create_dir_all(home_dir.join("nested"))?;
    fs::write(home_dir.join("nested/beta_alpha.md"), "alpha")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearch {
            request_id: request_id(552),
            params: codex::FuzzyFileSearchParams {
                query: "alpha".to_string(),
                roots: vec![home_dir.display().to_string()],
                cancellation_token: None,
            },
        },
    )
    .await?;
    let fuzzy: codex::FuzzyFileSearchResponse =
        read_typed_response(&mut ws, request_id(552)).await?;
    assert!(
        fuzzy
            .files
            .iter()
            .any(|file| file.path == "alpha_notes.txt" && file.file_name == "alpha_notes.txt"),
        "fuzzy search did not include alpha_notes.txt: {fuzzy:?}"
    );

    let session_id = format!("fuzzy-{}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionStart {
            request_id: request_id(553),
            params: codex::FuzzyFileSearchSessionStartParams {
                session_id: session_id.clone(),
                roots: vec![home_dir.display().to_string()],
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionStartResponse =
        read_typed_response(&mut ws, request_id(553)).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionUpdate {
            request_id: request_id(554),
            params: codex::FuzzyFileSearchSessionUpdateParams {
                session_id: session_id.clone(),
                query: "beta".to_string(),
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionUpdateResponse =
        read_typed_response(&mut ws, request_id(554)).await?;
    let fuzzy_update = read_fuzzy_file_search_update(&mut ws).await?;
    assert_eq!(fuzzy_update.session_id, session_id);
    assert_eq!(fuzzy_update.query, "beta");
    assert!(
        fuzzy_update
            .files
            .iter()
            .any(|file| file.path == "nested/beta_alpha.md"),
        "fuzzy search session update did not include nested/beta_alpha.md: {fuzzy_update:?}"
    );
    let fuzzy_completed = read_fuzzy_file_search_completed(&mut ws).await?;
    assert_eq!(fuzzy_completed.session_id, session_id);

    send_client_request(
        &mut ws,
        codex::ClientRequest::FuzzyFileSearchSessionStop {
            request_id: request_id(555),
            params: codex::FuzzyFileSearchSessionStopParams {
                session_id: session_id.clone(),
            },
        },
    )
    .await?;
    let _: codex::FuzzyFileSearchSessionStopResponse =
        read_typed_response(&mut ws, request_id(555)).await?;

    let repo = home_dir.join("git-repo");
    fs::create_dir_all(&repo)?;
    run_git_command(&repo, &["init"])?;
    fs::write(repo.join("tracked.txt"), "base\n")?;
    run_git_command(&repo, &["add", "tracked.txt"])?;
    run_git_command(
        &repo,
        &[
            "-c",
            "user.name=Defra Test",
            "-c",
            "user.email=defra-test@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    )?;
    fs::write(repo.join("tracked.txt"), "base\nchanged\n")?;
    fs::write(repo.join("untracked.txt"), "new\n")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::GitDiffToRemote {
            request_id: request_id(556),
            params: codex::GitDiffToRemoteParams { cwd: repo },
        },
    )
    .await?;
    let diff: codex::GitDiffToRemoteResponse =
        read_typed_response(&mut ws, request_id(556)).await?;
    assert!(
        diff.diff.contains("+changed"),
        "git diff did not include tracked change: {diff:?}"
    );
    assert!(
        diff.diff.contains("untracked.txt"),
        "git diff did not include untracked file: {diff:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_steer_queues_defra_request_on_active_turn() -> Result<()> {
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
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
        "second steering request should queue after the current DEFRA tail, not after the root turn"
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_defra_filesystem_tools_project_to_codex_items() -> Result<()> {
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
    let thread_name = format!("DEFRA live projection {}", Uuid::new_v4().simple());
    let goal_objective = format!("exercise live projection {}", Uuid::new_v4().simple());
    let git_sha = format!("live{}", Uuid::new_v4().simple());
    let git_branch = "codex-shim-live-projection".to_string();
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;
    let expected_cwd = smoke.home_dir.display().to_string();

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

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadMetadataUpdate {
            request_id: request_id(405),
            params: codex::ThreadMetadataUpdateParams {
                thread_id: thread_id.clone(),
                git_info: Some(codex::ThreadMetadataGitInfoUpdateParams {
                    sha: Some(Some(git_sha.clone())),
                    branch: Some(Some(git_branch.clone())),
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
        Some(git_sha.as_str())
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

    let projection_response = graphql_query(
        &smoke.graphql,
        &format!(
            r#"{{
                CodexThreadProjection(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    cwd
                    archived
                    loaded
                    memory_mode
                    name
                    settings_json
                    goal_json
                    git_info_json
                }}
                AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    behavior_id
                    status
                }}
            }}"#,
            escape_graphql_string(&thread_id),
            escape_graphql_string(&thread_id),
        ),
    )
    .await?;
    let projection = first_graphql_row(&projection_response, "CodexThreadProjection")?;
    assert_eq!(
        projection.get("session_id").and_then(Value::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        projection.get("cwd").and_then(Value::as_str),
        Some(expected_cwd.as_str())
    );
    assert_eq!(
        projection.get("archived").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        projection.get("loaded").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        projection.get("memory_mode").and_then(Value::as_str),
        Some("disabled")
    );
    assert_eq!(
        projection.get("name").and_then(Value::as_str),
        Some(thread_name.as_str())
    );
    let settings_json: Value = serde_json::from_str(
        projection
            .get("settings_json")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("projection missing settings_json: {projection}"))?,
    )
    .context("decoding stored live Codex thread settings")?;
    assert_eq!(
        settings_json.get("cwd").and_then(Value::as_str),
        Some(expected_cwd.as_str())
    );
    let goal_json: Value = serde_json::from_str(
        projection
            .get("goal_json")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("projection missing goal_json: {projection}"))?,
    )
    .context("decoding stored live Codex thread goal")?;
    assert_eq!(
        goal_json.get("objective").and_then(Value::as_str),
        Some(goal_objective.as_str())
    );
    assert_eq!(
        goal_json.get("tokenBudget").and_then(Value::as_i64),
        Some(321)
    );
    let git_info_json: Value = serde_json::from_str(
        projection
            .get("git_info_json")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("projection missing git_info_json: {projection}"))?,
    )
    .context("decoding stored live Codex thread git metadata")?;
    assert_eq!(
        git_info_json.get("sha").and_then(Value::as_str),
        Some(git_sha.as_str())
    );
    assert_eq!(
        git_info_json.get("branch").and_then(Value::as_str),
        Some(git_branch.as_str())
    );
    let session = first_graphql_row(&projection_response, "AgentSession")?;
    let expected_behavior_id = format!("{}:default", smoke.agent_did);
    assert_eq!(
        session.get("behavior_id").and_then(Value::as_str),
        Some(expected_behavior_id.as_str())
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
        .ok_or_else(|| anyhow!("live DEFRA-backed thread list did not include {thread_id}"))?;
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
            &["defra-agent"],
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_remote_frontend_keeps_client_codex_home_separate() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let client_codex_home = tempdir.path().join("existing-client-codex-home");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&client_codex_home)?;
    fs::write(
        client_codex_home.join("config.toml"),
        "# Existing user Codex config should remain client-side.\n",
    )?;

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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let expected_model_selection =
        defra_model_selection_id(&default_backend_id(&agent_did), &model_name);
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
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let expected_shim_home = home_dir.join(".defra-agent").join("codex-ui");
    let (_stdout, stderr) = serve.captured_output()?;
    assert!(
        stderr.contains(&format!(
            "Launch Codex with: codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{shim_port}/"
        )),
        "server guidance should use --remote without requiring CODEX_HOME; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("CODEX_HOME="),
        "server guidance should not instruct users to replace their existing Codex home; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("No CODEX_HOME override is required"),
        "server guidance should explain the client/server home split; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&expected_shim_home.to_string_lossy().to_string()),
        "server guidance should still identify the shim state dir; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(&client_codex_home.to_string_lossy().to_string()),
        "server guidance must not depend on or rewrite a user's local Codex home; stderr:\n{stderr}"
    );

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    assert_eq!(
        initialize.codex_home.as_path(),
        expected_shim_home.as_path(),
        "initialize codexHome is shim state, not the user's local Codex home"
    );
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(2),
            params: codex::ThreadStartParams {
                model: Some("client-local-model-from-existing-codex-config".to_string()),
                model_provider: Some("openai".to_string()),
                approval_policy: Some(codex::AskForApproval::OnRequest),
                sandbox: Some(codex::SandboxMode::ReadOnly),
                cwd: None,
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        thread_start.model, expected_model_selection,
        "Defra remote runtime should use the bound behavior model, not the client Codex model"
    );
    assert_eq!(thread_start.model_provider, "defra");
    assert_eq!(thread_start.approval_policy, codex::AskForApproval::Never);
    let expected_server_cwd = home_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", home_dir.display()))?;
    assert_eq!(
        thread_start.cwd.as_path(),
        expected_server_cwd.as_path(),
        "without a remote --cd override, the shim should keep its server-side cwd"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires stock codex CLI, expect, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_pty_smoke_uses_existing_client_codex_home_with_real_backend(
) -> Result<()> {
    require_command("codex")?;
    require_command("expect")?;
    let prompt_token = "PONGPTY";
    let smoke = start_live_codex_shim().await?;
    let client_codex_home = create_existing_client_codex_home(&smoke, "pty")?;
    assert_ne!(client_codex_home, smoke.codex_home);

    let transcript = smoke.tempdir.path().join("codex-pty.log");
    let expect_script = smoke.tempdir.path().join("codex-pty-smoke.expect");
    write_expect_smoke(
        &expect_script,
        &transcript,
        &client_codex_home,
        smoke.shim_port,
        prompt_token,
    )?;

    let output = Command::new("expect")
        .arg(&expect_script)
        .output()
        .context("running codex --remote PTY smoke through expect")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let transcript = fs::read_to_string(&transcript).unwrap_or_default();
    if !output.status.success() {
        let (server_stdout, server_stderr) = smoke._server.captured_output()?;
        let shim_trace = fs::read_to_string(&smoke.shim_trace).unwrap_or_default();
        bail!(
            "codex --remote PTY smoke failed\nstdout:\n{stdout}\nstderr:\n{stderr}\ntranscript:\n{transcript}\nserver stdout:\n{server_stdout}\nserver stderr:\n{server_stderr}\nshim trace:\n{shim_trace}"
        );
    }
    let token_search_text = terminal_token_search_text(&transcript);
    assert!(
        token_occurrences(&token_search_text, prompt_token) >= 2,
        "expected PTY transcript to contain an echoed prompt and assistant response for {prompt_token}\nstdout:\n{stdout}\nstderr:\n{stderr}\ntranscript:\n{transcript}"
    );
    let prompt = smoke_prompt(prompt_token);
    let (_request_id, _session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "thread/start", "turn/start"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires tmux, stock codex CLI, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_tmux_smoke_uses_existing_client_codex_home_with_real_backend(
) -> Result<()> {
    require_command("codex")?;
    if which("tmux").is_none() {
        eprintln!("skipping tmux smoke: tmux is not installed");
        return Ok(());
    }
    let prompt_token = "PONGTMUX";
    let smoke = start_live_codex_shim().await?;
    let client_codex_home = create_existing_client_codex_home(&smoke, "tmux")?;
    assert_ne!(client_codex_home, smoke.codex_home);
    let session = format!("defra-codex-smoke-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&client_codex_home),
        smoke.shim_port,
        shell_quote(&format!(
            "Reply with exactly this token and no extra words: {prompt_token}"
        )),
    );

    let new_status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, &command])
        .status()
        .context("starting tmux codex smoke session")?;
    if !new_status.success() {
        bail!("tmux new-session failed");
    }
    let transcript =
        wait_for_tmux_token_occurrences(&session, prompt_token, 2, Duration::from_secs(180))?;
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();
    let token_search_text = terminal_token_search_text(&transcript);
    assert!(
        token_occurrences(&token_search_text, prompt_token) >= 2,
        "expected tmux transcript to contain an echoed prompt and assistant response for {prompt_token}, got:\n{transcript}"
    );
    let prompt = smoke_prompt(prompt_token);
    let (_request_id, _session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "thread/start", "turn/start"],
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires tmux, stock codex CLI, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_tmux_multiturn_uses_existing_client_codex_home_with_real_backend(
) -> Result<()> {
    require_command("codex")?;
    if which("tmux").is_none() {
        eprintln!("skipping tmux multi-turn smoke: tmux is not installed");
        return Ok(());
    }
    let memory_token = "LIME7";
    let transformed_token = "MINT7";
    let first_prompt = multiturn_first_prompt(memory_token);
    let second_prompt = multiturn_second_prompt();
    let smoke = start_live_codex_shim().await?;
    let client_codex_home = create_existing_client_codex_home(&smoke, "tmux-multiturn")?;
    assert_ne!(client_codex_home, smoke.codex_home);
    let session = format!("defra-codex-multiturn-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&client_codex_home),
        smoke.shim_port,
        shell_quote(&first_prompt),
    );

    let new_status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, &command])
        .status()
        .context("starting tmux codex multi-turn smoke session")?;
    if !new_status.success() {
        bail!("tmux new-session failed");
    }

    let result: Result<()> = async {
        wait_for_tmux_token_occurrences(&session, "READY", 2, Duration::from_secs(180))?;
        let literal_status = Command::new("tmux")
            .args(["send-keys", "-t", &session, "-l", second_prompt])
            .status()
            .context("sending second prompt to tmux codex session")?;
        if !literal_status.success() {
            bail!("tmux send-keys second prompt failed");
        }
        std::thread::sleep(Duration::from_millis(1500));
        let enter_status = Command::new("tmux")
            .args(["send-keys", "-t", &session, "Enter"])
            .status()
            .context("submitting second prompt in tmux codex session")?;
        if !enter_status.success() {
            bail!("tmux send-keys Enter failed");
        }

        let transcript = wait_for_tmux_token_occurrences(
            &session,
            transformed_token,
            1,
            Duration::from_secs(180),
        )?;
        let token_search_text = terminal_token_search_text(&transcript);
        assert!(
            token_occurrences(&token_search_text, transformed_token) >= 1,
            "expected tmux transcript to contain transformed multi-turn response {transformed_token}, got:\n{transcript}"
        );
        let (_request_id, first_session_id, _behavior_id) =
            wait_for_request(&smoke.graphql, &smoke.agent_did, &first_prompt).await?;
        let (_request_id, second_session_id, _behavior_id) =
            wait_for_request(&smoke.graphql, &smoke.agent_did, second_prompt).await?;
        assert_eq!(first_session_id, second_session_id);
        assert_shim_trace_methods(&smoke.shim_trace, &["initialize", "thread/start"])?;
        assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/start", 2)?;
        Ok(())
    }
    .await;
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();
    result?;
    Ok(())
}

struct LiveCodexShim {
    tempdir: tempfile::TempDir,
    home_dir: std::path::PathBuf,
    codex_home: std::path::PathBuf,
    graphql: String,
    agent_did: String,
    shim_port: u16,
    shim_trace: std::path::PathBuf,
    _server: ServeProcess,
}

async fn start_live_codex_shim() -> Result<LiveCodexShim> {
    start_live_codex_shim_with_write_tools(false, None).await
}

fn create_existing_client_codex_home(
    smoke: &LiveCodexShim,
    label: &str,
) -> Result<std::path::PathBuf> {
    let codex_home = smoke
        .tempdir
        .path()
        .join(format!("client-codex-home-{label}"));
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating client Codex home {}", codex_home.display()))?;
    fs::write(
        codex_home.join("config.toml"),
        "# Existing user Codex config should remain client-side.\n",
    )
    .with_context(|| format!("writing client Codex config in {}", codex_home.display()))?;
    Ok(codex_home)
}

async fn start_live_codex_shim_with_write_tools(
    write_tools: bool,
    tool_root: Option<&std::path::Path>,
) -> Result<LiveCodexShim> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-live-{}", Uuid::new_v4().simple());
    let tool_root_string = tool_root.map(|root| root.to_string_lossy().to_string());
    let mut init_args = vec![
        "--agent-name",
        &agent_name,
        "--model-name",
        DEFAULT_MODEL_NAME,
        "--inference-url",
        DEFAULT_MODEL_ENDPOINT,
    ];
    if write_tools {
        init_args.push("--write-tools");
    }
    if let Some(tool_root) = &tool_root_string {
        init_args.push("--tool-root");
        init_args.push(tool_root.as_str());
    }
    let init = run_init_json(&home_dir, &init_args)?;
    let agent_did = agent_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let codex_home = home_dir.join(".defra-agent").join("codex-ui");
    let shim_trace = codex_home.join("log").join("codex-shim-events.jsonl");
    let mut server = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "250",
            "--codex-shim-timeout-secs",
            LIVE_CODEX_SHIM_TIMEOUT_SECS,
        ],
        &[(
            "RUST_LOG",
            "error,defra_agent_cli::commands::codex_shim=info",
        )],
    )?;
    wait_for_port(server_port, &mut server)?;
    wait_for_port(shim_port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    Ok(LiveCodexShim {
        codex_home,
        tempdir,
        home_dir,
        graphql,
        agent_did,
        shim_port,
        shim_trace,
        _server: server,
    })
}

async fn initialize_config_and_thread(
    ws: &mut ShimWebSocket,
    _home_dir: &std::path::Path,
) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(101),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-live-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(ws, request_id(101)).await?;
    send_client_notification(ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(102),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let _: codex::ConfigReadResponse = read_typed_response(ws, request_id(102)).await?;
    Ok(())
}

async fn start_thread(ws: &mut ShimWebSocket, home_dir: &std::path::Path) -> Result<String> {
    send_client_request(
        ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(103),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse = read_typed_response(ws, request_id(103)).await?;
    Ok(thread_start.thread.id)
}

async fn send_turn(ws: &mut ShimWebSocket, thread_id: &str, prompt: &str) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(104),
            params: codex::TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(ws, request_id(104)).await?;
    Ok(())
}

async fn seed_blank_materialized_completion(
    graphql: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let message_key = format!("{session_id}:blank-terminal");
    let blank_assistant = "\n\n\n";
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                sequence: 2,
                role: "assistant",
                content: "{blank_assistant}",
                timestamp: "{now}"
            }}) {{ _docID }}
            upsert_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                add: {{
                    response_key: "{request_id}",
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    token_count: 0,
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    created_at: "{now}",
                    completed_at: "{now}"
                }},
                update: {{
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        message_key = escape_graphql_string(&message_key),
        session_id = escape_graphql_string(session_id),
        blank_assistant = escape_graphql_string(blank_assistant),
        now = escape_graphql_string(&now),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

fn require_command(name: &str) -> Result<()> {
    if which(name).is_some() {
        Ok(())
    } else {
        bail!("{name} is required for this smoke test")
    }
}

fn run_git_command(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running git {} in {}", args.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn gh_is_authenticated() -> bool {
    Command::new("gh")
        .arg("auth")
        .arg("status")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(std::path::Path::new)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
}

fn workspace_root() -> Result<std::path::PathBuf> {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow!("unable to resolve workspace root from CARGO_MANIFEST_DIR"))
}

fn write_expect_smoke(
    script: &std::path::Path,
    transcript: &std::path::Path,
    client_codex_home: &std::path::Path,
    shim_port: u16,
    prompt_token: &str,
) -> Result<()> {
    let prompt = smoke_prompt(prompt_token);
    let token_match_regex = tcl_regex_terminal_tolerant_literal(prompt_token);
    let contents = format!(
        r#"set timeout 120
set env(CODEX_HOME) {{{codex_home}}}
set env(TERM) xterm-256color
stty rows 40 columns 120
log_user 0
spawn codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{shim_port}/ {{{prompt}}}
log_file -a {{{transcript}}}
set match_count 0
expect {{
  -ex "\033\[6n" {{
    send "\033\[24;1R"
    exp_continue
  }}
  -ex "\033\[?u" {{
    send "\033\[?0u"
    exp_continue
  }}
  -ex "\033\[c" {{
    send "\033\[?1;2c"
    exp_continue
  }}
  -ex "\033]10;?\033\\" {{
    send "\033]10;rgb:ffff/ffff/ffff\033\\"
    exp_continue
  }}
  -ex "\033]11;?\033\\" {{
    send "\033]11;rgb:0000/0000/0000\033\\"
    exp_continue
  }}
  -re {{{token_match_regex}}} {{
    incr match_count
    if {{$match_count >= 2}} {{
      after 2000
      send "\003"
      expect {{
        eof {{ exit 0 }}
        timeout {{ exit 0 }}
      }}
    }}
    exp_continue
  }}
  timeout {{
    send "\003"
    expect {{
      eof {{ exit 0 }}
      timeout {{ exit 0 }}
    }}
  }}
  eof {{ exit 2 }}
}}
"#,
        transcript = tcl_brace(transcript),
        codex_home = tcl_brace(client_codex_home),
        prompt = tcl_brace_str(&prompt),
        token_match_regex = tcl_brace_str(&token_match_regex),
    );
    fs::write(script, contents).with_context(|| format!("writing {}", script.display()))
}

fn smoke_prompt(prompt_token: &str) -> String {
    format!("Reply with exactly this token and no extra words: {prompt_token}")
}

fn multiturn_first_prompt(memory_token: &str) -> String {
    format!(
        "The project codeword for this conversation is {memory_token}. Reply with exactly READY and no extra words."
    )
}

fn multiturn_second_prompt() -> &'static str {
    "Take the project codeword I gave earlier, replace LIME with MINT, keep the digit, and reply with exactly the transformed codeword and no extra words."
}

fn assert_shim_trace_methods(path: &std::path::Path, methods: &[&str]) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    for method in methods {
        assert!(
            trace.contains(method),
            "expected shim trace to contain {method}, got:\n{trace}"
        );
    }
    Ok(())
}

fn assert_shim_trace_method_count_at_least(
    path: &std::path::Path,
    method: &str,
    minimum: usize,
) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    let count = trace.matches(method).count();
    assert!(
        count >= minimum,
        "expected shim trace to contain {method} at least {minimum} times, got {count}:\n{trace}"
    );
    Ok(())
}

fn wait_for_tmux_token_occurrences(
    session: &str,
    needle: &str,
    required_count: usize,
    timeout: Duration,
) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = String::new();
    loop {
        let output = Command::new("tmux")
            .args(["capture-pane", "-pt", session])
            .output()
            .context("capturing tmux pane")?;
        if output.status.success() {
            last = String::from_utf8_lossy(&output.stdout).into_owned();
            if token_occurrences(&terminal_token_search_text(&last), needle) >= required_count {
                return Ok(last);
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting for {required_count} occurrences of {needle} in tmux pane; last transcript:\n{last}"
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn shell_quote_path(path: &std::path::Path) -> String {
    shell_quote(&path.display().to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tcl_brace(path: &std::path::Path) -> String {
    tcl_brace_str(&path.display().to_string())
}

fn tcl_brace_str(value: &str) -> String {
    value.replace('\\', r"\\").replace('}', r"\}")
}

fn tcl_regex_terminal_tolerant_literal(value: &str) -> String {
    let mut regex = String::from("(?s)");
    for (index, ch) in value.chars().enumerate() {
        if index > 0 {
            regex.push_str(".*");
        }
        if matches!(
            ch,
            '.' | '\\'
                | '+'
                | '*'
                | '?'
                | '['
                | '^'
                | ']'
                | '$'
                | '('
                | ')'
                | '{'
                | '}'
                | '='
                | '!'
                | '<'
                | '>'
                | '|'
                | ':'
                | '-'
        ) {
            regex.push('\\');
        }
        regex.push(ch);
    }
    regex
}

fn terminal_token_search_text(value: &str) -> String {
    terminal_visible_text(value)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

fn token_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn terminal_visible_text(value: &str) -> String {
    let mut visible = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            skip_escape_sequence(&mut chars);
        } else if ch == '\r' || ch == '\n' {
            visible.push('\n');
        } else if !ch.is_control() {
            visible.push(ch);
        }
    }
    visible
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            let mut saw_escape = false;
            for ch in chars.by_ref() {
                if ch == '\u{7}' || (saw_escape && ch == '\\') {
                    break;
                }
                saw_escape = ch == '\u{1b}';
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn request_id(value: i64) -> codex::RequestId {
    codex::RequestId::Integer(value)
}

async fn send_client_request(ws: &mut ShimWebSocket, request: codex::ClientRequest) -> Result<()> {
    let value = serde_json::to_value(request).context("serializing Codex client request")?;
    let request: codex::JSONRPCRequest =
        serde_json::from_value(value).context("building JSON-RPC request")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

async fn send_raw_client_request(
    ws: &mut ShimWebSocket,
    request_id: codex::RequestId,
    method: &str,
    params: Value,
) -> Result<()> {
    let request: codex::JSONRPCRequest = serde_json::from_value(json!({
        "id": request_id,
        "method": method,
        "params": params,
    }))
    .with_context(|| format!("building raw JSON-RPC request for {method}"))?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

async fn send_client_notification(
    ws: &mut ShimWebSocket,
    notification: codex::ClientNotification,
) -> Result<()> {
    let value =
        serde_json::to_value(notification).context("serializing Codex client notification")?;
    let notification: codex::JSONRPCNotification =
        serde_json::from_value(value).context("building JSON-RPC notification")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Notification(notification)).await
}

async fn write_jsonrpc(ws: &mut ShimWebSocket, message: codex::JSONRPCMessage) -> Result<()> {
    let text = serde_json::to_string(&message).context("encoding JSON-RPC message")?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .context("sending JSON-RPC websocket message")
}

async fn read_typed_response<T>(ws: &mut ShimWebSocket, expected_id: codex::RequestId) -> Result<T>
where
    T: DeserializeOwned,
{
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                return serde_json::from_value(response.result)
                    .context("decoding typed Codex response");
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for request {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!("unexpected JSON-RPC message while waiting for {expected_id}: {other:?}")
            }
        }
    }
}

async fn read_error_response(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<codex::JSONRPCErrorError> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                return Ok(error.error);
            }
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                bail!("expected JSON-RPC error for {expected_id}, got response {response:?}");
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!(
                    "unexpected JSON-RPC message while waiting for error {expected_id}: {other:?}"
                )
            }
        }
    }
}

async fn read_turn_started(ws: &mut ShimWebSocket) -> Result<codex::TurnStartedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::TurnStarted(started) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(started);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

async fn read_fuzzy_file_search_update(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionUpdatedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionUpdated(update) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(update);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

async fn read_fuzzy_file_search_completed(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionCompletedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(completed);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

#[derive(Debug)]
struct TurnCapture {
    text: String,
    turn: codex::Turn,
    started_tools: Vec<String>,
    completed_tool_ids: Vec<String>,
    completed_tools: Vec<String>,
    turn_completed_tool_ids: Vec<String>,
    event_order: Vec<TurnStreamEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnStreamEvent {
    AgentDelta,
    ToolStarted,
    ToolCompleted,
}

async fn read_turn_to_completion(ws: &mut ShimWebSocket) -> Result<(String, codex::Turn)> {
    let capture = read_turn_capture(ws).await?;
    Ok((capture.text, capture.turn))
}

async fn read_turn_capture(ws: &mut ShimWebSocket) -> Result<TurnCapture> {
    let mut text = String::new();
    let mut started_tools = Vec::new();
    let mut completed_tool_ids = Vec::new();
    let mut completed_tools = Vec::new();
    let mut event_order = Vec::new();
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::AgentMessageDelta(delta) => {
                        if !delta.delta.is_empty() {
                            event_order.push(TurnStreamEvent::AgentDelta);
                        }
                        text.push_str(&delta.delta);
                    }
                    codex::ServerNotification::ItemStarted(started) => match started.item {
                        codex::ThreadItem::McpToolCall { tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { command, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(command);
                        }
                        _ => {}
                    },
                    codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                        codex::ThreadItem::McpToolCall { id, tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { id, command, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(command);
                        }
                        _ => {}
                    },
                    codex::ServerNotification::TurnCompleted(completed) => {
                        let turn_completed_tool_ids = mcp_tool_ids(&completed.turn);
                        return Ok(TurnCapture {
                            text,
                            turn: completed.turn,
                            started_tools,
                            completed_tool_ids,
                            completed_tools,
                            turn_completed_tool_ids,
                            event_order,
                        });
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

fn assert_text_contains_all_case_insensitive(text: &str, label: &str, needles: &[&str]) {
    let lower = text.to_ascii_lowercase();
    for needle in needles {
        assert!(
            lower.contains(&needle.to_ascii_lowercase()),
            "{label} response did not contain {needle:?}:\n{text}"
        );
    }
}

fn turn_had_tool_before_later_agent_text(capture: &TurnCapture) -> bool {
    let mut saw_tool = false;
    for event in &capture.event_order {
        match event {
            TurnStreamEvent::AgentDelta if saw_tool => return true,
            TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted => saw_tool = true,
            TurnStreamEvent::AgentDelta => {}
        }
    }
    false
}

fn turn_had_tool_after_final_agent_text(capture: &TurnCapture) -> bool {
    let Some(final_agent_index) = capture
        .event_order
        .iter()
        .rposition(|event| *event == TurnStreamEvent::AgentDelta)
    else {
        return false;
    };
    capture.event_order[final_agent_index + 1..]
        .iter()
        .any(|event| {
            matches!(
                event,
                TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted
            )
        })
}

fn mcp_tool_ids(turn: &codex::Turn) -> Vec<String> {
    turn.items
        .iter()
        .filter_map(|item| match item {
            codex::ThreadItem::McpToolCall { id, .. } => Some(id.clone()),
            codex::ThreadItem::CommandExecution { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn assert_turn_has_user_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::UserMessage { content, .. } => {
                content.iter().any(|input| match input {
                    codex::UserInput::Text { text, .. } => text.contains(expected),
                    _ => false,
                })
            }
            _ => false,
        }),
        "turn {} did not include user text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

fn assert_turn_has_agent_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::AgentMessage { text, .. } => text.contains(expected),
            _ => false,
        }),
        "turn {} did not include agent text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

async fn wait_for_request_metadata(
    graphql: &str,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, Value)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{}" }},
                        content: {{ _eq: "{}" }}
                    }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    metadata
                }}
            }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(content),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing session_id: {row}"))?;
            let metadata_raw = row
                .get("metadata")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing metadata: {row}"))?;
            let metadata = serde_json::from_str::<Value>(metadata_raw)
                .with_context(|| format!("decoding AgentRequest metadata: {metadata_raw}"))?;
            return Ok((request_id.to_string(), session_id.to_string(), metadata));
        }

        if std::time::Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest metadata for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn read_jsonrpc(ws: &mut ShimWebSocket) -> Result<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .context("timed out waiting for Codex shim websocket message")?
            .ok_or_else(|| anyhow!("Codex shim websocket closed"))?
            .context("reading Codex shim websocket message")?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("decoding binary websocket payload as UTF-8")?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => bail!("Codex shim websocket closed: {close:?}"),
            WsMessage::Frame(_) => bail!("unexpected raw websocket frame"),
        };
        return serde_json::from_str(&text)
            .with_context(|| format!("decoding JSON-RPC message: {text}"));
    }
}

fn server_notification_from_jsonrpc(
    notification: codex::JSONRPCNotification,
) -> Result<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .context("decoding Codex server notification")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_refuses_to_start_when_bound_behavior_is_missing() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let _init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    // Point the shim at a behavior_id that doesn't exist. The startup
    // precondition should reject it before opening the WebSocket listener.
    // Watch the child directly rather than wait_for_port — under parallel
    // execution another test could already be listening on the same port,
    // which would mask the expected exit.
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-behavior-id",
            "behavior-that-does-not-exist",
        ],
        &[],
    )?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = serve.child.try_wait().context("waiting for serve child")? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = serve.child.kill();
            bail!("server did not exit within 30s after misconfigured shim startup");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        !status.success(),
        "expected server to exit non-zero with misconfigured shim, got {status}"
    );
    let (_stdout, stderr) = serve.captured_output()?;
    assert!(
        stderr.contains("behavior-that-does-not-exist"),
        "expected stderr to name the missing behavior id; got:\n{stderr}"
    );
    assert!(
        stderr.contains("AgentBehavior") || stderr.contains("behavior"),
        "expected stderr to mention AgentBehavior; got:\n{stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_model_list_enumerates_backend_models() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_backend_id = default_backend_id(&agent_did);
    let default_model_selection = defra_model_selection_id(&default_backend_id, &model_name);
    let extra_model_name = format!("mock-codex-shim-extra-model-{}", Uuid::new_v4().simple());
    let extra_endpoint = MockChatEndpoint::start(&extra_model_name, "irrelevant")?;
    let extra_backend_id = format!("extra-backend-{}", Uuid::new_v4().simple());
    let extra_model_selection = defra_model_selection_id(&extra_backend_id, &extra_model_name);
    let duplicate_backend_id = format!("duplicate-backend-{}", Uuid::new_v4().simple());
    let duplicate_model_selection = defra_model_selection_id(&duplicate_backend_id, &model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let create_extra_backend = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{extra_backend_id}",
                name: "Extra Backend",
                provider_kind: "OpenAiCompatible",
                endpoint: "{}",
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true,
                models: ["{extra_model_name}"],
                probe_status: "healthy"
            }}) {{ _docID }}
            create_duplicate: create_InferenceBackend(input: {{
                backend_id: "{duplicate_backend_id}",
                name: "Duplicate Backend",
                provider_kind: "OpenAiCompatible",
                endpoint: "{}",
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true,
                models: ["{model_name}"],
                probe_status: "healthy"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(extra_endpoint.endpoint()),
        escape_graphql_string(extra_endpoint.endpoint())
    );
    graphql_query(&graphql, &create_extra_backend).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        codex::ClientRequest::ModelList {
            request_id: request_id(2),
            params: codex::ModelListParams::default(),
        },
    )
    .await?;
    let model_list: codex::ModelListResponse = read_typed_response(&mut ws, request_id(2)).await?;

    let ids: Vec<&str> = model_list
        .data
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(
        ids.contains(&default_model_selection.as_str()),
        "expected default model selection {default_model_selection} in model list; got {ids:?}"
    );
    assert!(
        ids.contains(&extra_model_selection.as_str()),
        "expected extra model selection {extra_model_selection} in model list; got {ids:?}"
    );
    assert!(
        ids.contains(&duplicate_model_selection.as_str()),
        "expected duplicate model selection {duplicate_model_selection} in model list; got {ids:?}"
    );
    let default_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == default_model_selection)
        .expect("default model present");
    assert_eq!(default_entry.model, default_model_selection);
    assert_eq!(default_entry.display_name, model_name);
    assert!(
        default_entry.is_default,
        "default model should be flagged as isDefault"
    );
    let extra_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == extra_model_selection)
        .expect("extra model present");
    assert!(
        !extra_entry.is_default,
        "non-default model must not be flagged isDefault"
    );
    let duplicate_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == duplicate_model_selection)
        .expect("duplicate backend model present");
    assert_eq!(duplicate_entry.display_name, model_name);
    assert!(
        !duplicate_entry.is_default,
        "duplicate backend model must not be flagged isDefault"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_read_reflects_doc_mutation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let default_backend_id = default_backend_id(&agent_did);
    let alt_model_name = format!("alt-model-{}", Uuid::new_v4().simple());
    let alt_model_selection = defra_model_selection_id(&default_backend_id, &alt_model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let switch_behavior = format!(
        r#"mutation {{
            update_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                input: {{ model_name: "{alt_model_name}" }}
            ) {{ _docID }}
        }}"#
    );
    graphql_query(&graphql, &switch_behavior).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        Some(alt_model_selection.as_str()),
        "ConfigRead should reflect the doc-mutated backend-qualified model selection"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_model_mutates_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let original_profile_id = format!("{agent_did}:default-profile");
    let alt_model_name = format!("mock-codex-shim-alt-model-{}", Uuid::new_v4().simple());
    let alt_endpoint = MockChatEndpoint::start(&alt_model_name, "irrelevant")?;
    let alt_backend_id = format!("alt-backend-{}", Uuid::new_v4().simple());
    let alt_model_selection = defra_model_selection_id(&alt_backend_id, &alt_model_name);

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let create_alt_backend = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{alt_backend_id}",
                name: "Alt Backend",
                provider_kind: "OpenAiCompatible",
                endpoint: "{}",
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true,
                models: ["{alt_model_name}"],
                probe_status: "healthy"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(alt_endpoint.endpoint())
    );
    graphql_query(&graphql, &create_alt_backend).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key_path: "model".to_string(),
                value: serde_json::Value::String(alt_model_selection),
                merge_strategy: codex::MergeStrategy::Replace,
                file_path: None,
                expected_version: None,
            },
        },
    )
    .await?;
    let _write: codex::ConfigWriteResponse = read_typed_response(&mut ws, request_id(2)).await?;

    // Verify the AgentBehavior doc was updated to the selected backend model
    // while keeping the existing inference profile limits.
    let resp = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ backend_id model_name inference_profile_id }}
            }}"#
        ),
    )
    .await?;
    let stored_backend = resp
        .pointer("/data/AgentBehavior/0/backend_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_backend, alt_backend_id,
        "AgentBehavior.backend_id should reflect ConfigValueWrite"
    );
    let stored_model = resp
        .pointer("/data/AgentBehavior/0/model_name")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_model, alt_model_name,
        "AgentBehavior.model_name should reflect ConfigValueWrite"
    );
    let stored_profile = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_profile, original_profile_id,
        "AgentBehavior.inference_profile_id should remain unchanged by model selection"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_rejects_unknown_model() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let original_backend_id = format!("{agent_did}:backend");
    let original_profile_id = format!("{agent_did}:default-profile");

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key_path: "model".to_string(),
                value: serde_json::Value::String("definitely-not-real".to_string()),
                merge_strategy: codex::MergeStrategy::Replace,
                file_path: None,
                expected_version: None,
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(2)).await?;
    assert!(
        error.message.contains("model") && error.message.contains("not found"),
        "expected error to mention missing model; got: {}",
        error.message
    );

    let resp = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ backend_id model_name inference_profile_id }}
            }}"#
        ),
    )
    .await?;
    let stored_backend = resp
        .pointer("/data/AgentBehavior/0/backend_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_backend, original_backend_id,
        "behavior backend_id must remain unchanged after rejected write"
    );
    let stored_model = resp
        .pointer("/data/AgentBehavior/0/model_name")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_model, model_name,
        "behavior model_name must remain unchanged after rejected write"
    );
    let stored_profile = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored_profile, original_profile_id,
        "behavior inference_profile_id must remain unchanged after rejected write"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_does_not_clobber_session_behavior_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Pre-seed an AgentSession with a foreign agent_name pinned to the default behavior.
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{session_id}",
                    agent_name: "preexisting",
                    behavior_id: "{default_behavior_id}",
                    status: "active"
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;

    // Trigger ensure_agent_session by resuming the pre-seeded session id.
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
    // Drain whichever response shape comes back; we only care about doc state.
    let _ = read_jsonrpc(&mut ws).await?;

    let resp = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentSession(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    limit: 1
                ) {{ agent_name behavior_id }}
            }}"#
        ),
    )
    .await?;
    let preserved_agent_name = resp
        .pointer("/data/AgentSession/0/agent_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let preserved_behavior_id = resp
        .pointer("/data/AgentSession/0/behavior_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        preserved_agent_name, "preexisting",
        "agent_name must not be clobbered by the shim's session upsert"
    );
    assert_eq!(
        preserved_behavior_id, default_behavior_id,
        "behavior_id must remain pinned to its create-time value"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_rejects_resume_with_mismatched_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
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
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());
    let foreign_behavior_id = "some-other-behavior".to_string();

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Seed an AgentSession pinned to a behavior the shim isn't bound to.
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{session_id}",
                    agent_name: "foreign",
                    behavior_id: "{foreign_behavior_id}",
                    status: "active"
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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
        error.message.contains(&foreign_behavior_id),
        "expected mismatch error to name the foreign behavior id; got: {}",
        error.message
    );
    assert!(
        error.message.contains("pinned"),
        "expected error to use 'pinned' wording; got: {}",
        error.message
    );
    Ok(())
}

/// End-to-end (#340 slice 4): add a skill via the `config skill` CLI, then list
/// and enable/disable it through the Codex shim protocol (skills/list +
/// skills/config/write) — the management flow from the Codex CLI.
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Add a principal-scoped skill via the CLI management command.
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

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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

    // skills/list surfaces the skill, enabled, with principal scope -> System.
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

    // skills/config/write disables it by name.
    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsConfigWrite {
            request_id: request_id(3),
            params: codex::SkillsConfigWriteParams {
                path: None,
                name: Some("Research".to_string()),
                enabled: false,
            },
        },
    )
    .await?;
    let write: codex::SkillsConfigWriteResponse =
        read_typed_response(&mut ws, request_id(3)).await?;
    assert!(
        !write.effective_enabled,
        "config write should report disabled"
    );

    // skills/list reflects the disable.
    send_client_request(
        &mut ws,
        codex::ClientRequest::SkillsList {
            request_id: request_id(4),
            params: codex::SkillsListParams::default(),
        },
    )
    .await?;
    let list: codex::SkillsListResponse = read_typed_response(&mut ws, request_id(4)).await?;
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

/// End-to-end "agent discovers a skill in a live conversation" (#340): with the
/// agent already running, `config skill add` reconciles the runtime live (no
/// restart), and a subsequent turn carries the skill's catalog entry into the
/// request the agent sends the model (progressive disclosure — the model would
/// then call load_skill for the body).
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    // Add a principal-scoped skill LIVE; the catalog phrase is distinctive.
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
    // The live skill add must reconcile the running runtime (no restart).
    wait_for_runtime_quiescence(&graphql, &agent_did, gen0 + 1, Duration::from_secs(2)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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

    // The skill catalog (name + load_skill mandate) must have reached the model.
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

/// End-to-end proof that an EXPLICIT Codex skill selection (`UserInput::Skill`,
/// the skill "pill") deterministically activates the skill (#340). The shim
/// forwards only the id; the RUNTIME resolves it against the behavior's
/// effective set and injects the body as a per-turn system reminder (rather than
/// relying on the model to pull it). A skill-only turn (no text) must (a) not be
/// rejected as empty and (b) carry the skill body to the model.
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    // A skill with a distinctive instruction body (the injected-body marker).
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
    // The runtime resolves the explicit selection from its effective set, so the
    // principal-scoped skill must reconcile into the running snapshot first.
    wait_for_runtime_quiescence(&graphql, &agent_did, gen0 + 1, Duration::from_secs(2)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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

    // Turn input is ONLY the skill selection (no text) — proves it isn't
    // rejected as empty and that the body is injected.
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(3),
            params: codex::TurnStartParams {
                thread_id: thread_start.thread.id.clone(),
                input: vec![codex::UserInput::Skill {
                    name: "Injectable".to_string(),
                    path: std::path::PathBuf::from("/defra/skills/inject-skill"),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(3)).await?;
    let (_text, completed) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed.status, codex::TurnStatus::Completed);

    // The full skill body must have reached the model, wrapped as a <skill> block.
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

/// An explicit Codex skill selection must still respect the bound behavior's
/// effective set (D5): a behavior-scoped skill NOT opted into the bound behavior
/// (empty skill_refs) cannot be force-activated via the pill (#340). Privilege
/// scoping — the Codex UI lists all the agent's skills, but selecting one the
/// behavior didn't opt into must not inject it.
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // A BEHAVIOR-scoped skill, enabled, but NOT referenced by the bound behavior
    // (skill_refs is empty by default) -> not in its effective set.
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

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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

    // Select the unscoped skill, with text so the turn is non-empty regardless.
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
                        path: std::path::PathBuf::from("/defra/skills/unscoped-skill"),
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

    // The un-opted-in skill body must NOT have been injected.
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

/// End-to-end proof that a Codex-shim-driven `skills/config/write` disable
/// reconciles a RUNNING agent without a restart (#340): the shim commits the
/// toggle in a transaction, the COMMIT wakes the control watcher, the runtime
/// fingerprint changes (skills now contribute to `AgentBehavior`'s Debug), the
/// generation bumps, and the disabled skill's catalog entry stops reaching the
/// model on the next turn. This closes the gap where the shim's enable/disable
/// used an auto-committed mutation (no `Update` event) and only took effect on
/// restart.
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let gen0 = wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    // Add a principal-scoped skill LIVE (enabled) so it composes into the prompt.
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

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
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

    // Turn 1: the enabled skill's catalog entry reaches the model.
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

    // Disable the skill THROUGH THE CODEX SHIM (skills/config/write).
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

    // The shim's committed toggle must reconcile the running runtime (no restart).
    wait_for_runtime_quiescence(&graphql, &agent_did, gen1 + 1, Duration::from_secs(2)).await?;

    // Turn 2: the disabled skill's catalog entry no longer reaches the model.
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

/// CLI management round-trip for the `config skill` surface that the other
/// tests don't exercise directly: disable -> enable -> rm, verified through
/// `config skill show`/`list` (#340). Covers `skill_set_enabled` and `skill_rm`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_skill_cli_disable_enable_and_rm_round_trip() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-skill-crud-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "ok")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-crud-{}", Uuid::new_v4().simple());
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

    let mut serve = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
    wait_for_port(server_port, &mut serve)?;

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
            "research",
            "--scope",
            "principal",
            "--name",
            "Research",
            "--description",
            "Find and cite sources",
            "--instructions",
            "Always cite your sources.",
            "--tool-ref",
            "web_search",
        ],
    )?;

    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(show.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        show.get("tool_refs")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "tool_ref should be stored on add"
    );

    // Re-add without --tool-ref must CLEAR the list (upsert update writes null),
    // not leave the stale ["web_search"] in place.
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
    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    let tool_refs_empty = match show.get("tool_refs") {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        _ => false,
    };
    assert!(
        tool_refs_empty,
        "re-add without --tool-ref must clear tool_refs; got {:?}",
        show.get("tool_refs")
    );

    // disable
    let disabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "disable",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(disabled.get("updated").and_then(Value::as_u64), Some(1));
    assert_eq!(
        disabled.get("enabled").and_then(Value::as_bool),
        Some(false)
    );
    let show = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "show",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(show.get("enabled").and_then(Value::as_bool), Some(false));

    // re-enable
    let enabled = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "enable",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(enabled.get("enabled").and_then(Value::as_bool), Some(true));

    // rm, then it's gone from list
    let removed = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "rm",
            "--graphql",
            &graphql,
            "--skill-id",
            "research",
        ],
    )?;
    assert_eq!(removed.get("deleted").and_then(Value::as_u64), Some(1));
    let list = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "list",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    assert_eq!(list.get("count").and_then(Value::as_u64), Some(0));

    Ok(())
}

/// Real-world round-trip (#340 slice 5): import the NousResearch/hermes-agent
/// skill tree (~177 SKILL.md files), export it back to SKILL.md, and re-import
/// the export. Gated on the hermes skills directory existing (override with
/// HERMES_SKILLS_DIR); skipped otherwise so CI stays green without that checkout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_skill_import_export_roundtrip_hermes() -> Result<()> {
    let hermes_dir = std::env::var("HERMES_SKILLS_DIR").unwrap_or_else(|_| {
        "/Users/johnzampolin/go/src/github.com/NousResearch/hermes-agent/skills".to_string()
    });
    if !std::path::Path::new(&hermes_dir).is_dir() {
        eprintln!("skipping config_skill_import_export_roundtrip_hermes: {hermes_dir} not found");
        return Ok(());
    }

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-skill-import-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "ok")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-skill-import-{}", Uuid::new_v4().simple());
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

    let mut serve = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Import the hermes skill tree.
    let imported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "import",
            &hermes_dir,
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--scope",
            "behavior",
        ],
    )?;
    let imported_count = imported
        .get("imported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert!(
        imported_count >= 50,
        "expected to import many hermes skills, got {imported_count}: {imported}"
    );

    // List reflects the distinct skills (≤ import count if dir names collide).
    let listed = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "list",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    let listed_count = listed.get("count").and_then(Value::as_u64).unwrap_or(0);
    assert!(
        listed_count >= 50 && listed_count <= imported_count,
        "list count {listed_count}"
    );

    // Export back to a SKILL.md tree.
    let out_dir = tempdir.path().join("export");
    let exported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "export",
            out_dir.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    let exported_count = exported
        .get("exported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        exported_count, listed_count,
        "export count must match distinct skills"
    );
    assert!(
        out_dir.join("notion").join("SKILL.md").is_file(),
        "exported notion/SKILL.md should exist"
    );

    // Re-import the export: round-trips cleanly (same skill_ids upsert in place).
    let reimported = run_cli_json(
        &home_dir,
        &[
            "config",
            "skill",
            "import",
            out_dir.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--scope",
            "behavior",
        ],
    )?;
    let reimported_count = reimported
        .get("imported_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        reimported_count, exported_count,
        "re-import of export must round-trip"
    );

    Ok(())
}
