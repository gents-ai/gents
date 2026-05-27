mod support;
use support::*;

use std::fs;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use codex_app_server_protocol as codex;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type ShimWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const LIVE_CODEX_SHIM_TIMEOUT_SECS: &str = "900";

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
    assert_eq!(config.config.model.as_deref(), Some(model_name.as_str()));

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
                include_turns: false,
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
#[ignore = "requires stock codex CLI, expect, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_pty_smoke_uses_real_backend() -> Result<()> {
    require_command("codex")?;
    require_command("expect")?;
    let prompt_token = "PONGPTY";
    let smoke = start_live_codex_shim().await?;

    let transcript = smoke.tempdir.path().join("codex-pty.log");
    let expect_script = smoke.tempdir.path().join("codex-pty-smoke.expect");
    write_expect_smoke(
        &expect_script,
        &transcript,
        &smoke.codex_home,
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
async fn stock_codex_remote_tmux_smoke_uses_real_backend() -> Result<()> {
    require_command("codex")?;
    if which("tmux").is_none() {
        eprintln!("skipping tmux smoke: tmux is not installed");
        return Ok(());
    }
    let prompt_token = "PONGTMUX";
    let smoke = start_live_codex_shim().await?;
    let session = format!("defra-codex-smoke-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&smoke.codex_home),
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
async fn stock_codex_remote_tmux_multiturn_uses_real_backend() -> Result<()> {
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
    let session = format!("defra-codex-multiturn-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&smoke.codex_home),
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
            "--codex-shim-model",
            DEFAULT_MODEL_NAME,
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

fn require_command(name: &str) -> Result<()> {
    if which(name).is_some() {
        Ok(())
    } else {
        bail!("{name} is required for this smoke test")
    }
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
    codex_home: &std::path::Path,
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
        codex_home = tcl_brace(codex_home),
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
