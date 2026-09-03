mod support;
use support::*;

#[path = "cli_codex_shim/helpers/live.rs"]
mod live_helpers;
use live_helpers::*;
#[path = "cli_codex_shim/helpers/fixtures.rs"]
mod fixture_helpers;
use fixture_helpers::*;
#[path = "cli_codex_shim/helpers/terminal.rs"]
mod terminal_helpers;
use terminal_helpers::workspace_root;
use terminal_helpers::*;
#[path = "cli_codex_shim/helpers/wire.rs"]
mod wire_helpers;
use wire_helpers::*;
#[path = "cli_codex_shim/helpers/capture.rs"]
mod capture_helpers;
use capture_helpers::*;

use std::fs;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use gents::subagent_target_entry;
use gents_codex_protocol as codex;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type ShimWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const LIVE_CODEX_SHIM_TIMEOUT_SECS: &str = "900";

fn gents_model_selection_id(backend_id: &str, model_name: &str) -> String {
    format!("{backend_id}::{model_name}")
}

fn default_backend_id(agent_did: &str) -> String {
    format!("{agent_did}:backend")
}

#[path = "cli_codex_shim/background_continuations.rs"]
mod background_continuations;
#[path = "cli_codex_shim/behavior_scope.rs"]
mod behavior_scope;
#[path = "cli_codex_shim/host_runtime.rs"]
mod host_runtime;
#[path = "cli_codex_shim/live_backend.rs"]
mod live_backend;
#[path = "cli_codex_shim/model_configuration.rs"]
mod model_configuration;
#[path = "cli_codex_shim/remote_client.rs"]
mod remote_client;
#[path = "cli_codex_shim/server_lifecycle.rs"]
mod server_lifecycle;
#[path = "cli_codex_shim/skills_projection.rs"]
mod skills_projection;
#[path = "cli_codex_shim/skills_runtime.rs"]
mod skills_runtime;
#[path = "cli_codex_shim/subagents.rs"]
mod subagents;
#[path = "cli_codex_shim/thread_listing.rs"]
mod thread_listing;
#[path = "cli_codex_shim/thread_metadata.rs"]
mod thread_metadata;
#[path = "cli_codex_shim/turn_control.rs"]
mod turn_control;
#[path = "cli_codex_shim/turn_streaming.rs"]
mod turn_streaming;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_goal_round_trip_survives_shim_restart() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-goal-restart-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "unused")?;
    let server_port = allocate_port()?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-goal-restart-{}", Uuid::new_v4().simple());
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
    let server_args = [
        "--codex-shim-port",
        shim_port_string.as_str(),
        "--codex-shim-poll-ms",
        "50",
    ];

    let mut serve = spawn_server_with_env(&home_dir, server_port, &server_args, &[])?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/")).await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(119),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "materialize the canonical session".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(119)).await?;
    let _ = read_turn_capture(&mut ws).await?;
    let objective = format!("survive restart {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalSet {
            request_id: request_id(120),
            params: codex::ThreadGoalSetParams {
                thread_id: thread_id.clone(),
                objective: Some(objective.clone()),
                status: Some(codex::ThreadGoalStatus::Active),
                token_budget: Some(Some(12_345)),
            },
        },
    )
    .await?;
    let set: codex::ThreadGoalSetResponse = read_typed_response(&mut ws, request_id(120)).await?;
    assert_eq!(set.goal.objective, objective);
    ws.close(None).await?;
    drop(serve);

    let mut restarted = spawn_server_with_env(&home_dir, server_port, &server_args, &[])?;
    wait_for_port(server_port, &mut restarted)?;
    wait_for_port(shim_port, &mut restarted)?;
    restarted
        .capturing(wait_for_runtime_ready(
            &graphql,
            &agent_did,
            Duration::from_secs(30),
        ))
        .await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/")).await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalGet {
            request_id: request_id(121),
            params: codex::ThreadGoalGetParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let get: codex::ThreadGoalGetResponse = read_typed_response(&mut ws, request_id(121)).await?;
    let goal = get.goal.context("goal disappeared across shim restart")?;
    assert_eq!(goal.thread_id, thread_id);
    assert_eq!(goal.objective, objective);
    assert_eq!(goal.status, codex::ThreadGoalStatus::Active);
    assert_eq!(goal.token_budget, Some(12_345));

    let foreign_thread_id = format!("foreign-goal-{}", Uuid::new_v4().simple());
    restarted
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_Goal(input: {{
                    goal_id: "foreign-goal",
                    session_id: "{}",
                    agent_did: "{}",
                    objective: "belongs to another surface",
                    status: "active",
                    created_at: "2026-07-16T00:00:00Z"
                }}) {{ _docID }}
            }}"#,
                escape_graphql_string(&foreign_thread_id),
                escape_graphql_string(&agent_did),
            ),
        ))
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalGet {
            request_id: request_id(122),
            params: codex::ThreadGoalGetParams {
                thread_id: foreign_thread_id.clone(),
            },
        },
    )
    .await?;
    let foreign_get: codex::ThreadGoalGetResponse =
        read_typed_response(&mut ws, request_id(122)).await?;
    assert!(foreign_get.goal.is_none());
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalClear {
            request_id: request_id(123),
            params: codex::ThreadGoalClearParams {
                thread_id: foreign_thread_id.clone(),
            },
        },
    )
    .await?;
    let foreign_clear: codex::ThreadGoalClearResponse =
        read_typed_response(&mut ws, request_id(123)).await?;
    assert!(!foreign_clear.cleared);
    let foreign_rows = restarted
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{ Goal(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ goal_id }} }}"#,
                escape_graphql_string(&foreign_thread_id)
            ),
        ))
        .await?;
    assert_eq!(
        foreign_rows
            .pointer("/data/Goal")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    restarted
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"mutation {{
                create_Goal(input: {{
                    goal_id: "duplicate-goal",
                    session_id: "{}",
                    agent_did: "{}",
                    objective: "replicated twin",
                    status: "paused",
                    created_at: "2026-07-16T00:00:01Z"
                }}) {{ _docID }}
            }}"#,
                escape_graphql_string(&thread_id),
                escape_graphql_string(&agent_did),
            ),
        ))
        .await?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadGoalClear {
            request_id: request_id(124),
            params: codex::ThreadGoalClearParams {
                thread_id: thread_id.clone(),
            },
        },
    )
    .await?;
    let cleared: codex::ThreadGoalClearResponse =
        read_typed_response(&mut ws, request_id(124)).await?;
    assert!(cleared.cleared);
    let cleared_rows = restarted
        .capturing(graphql_query(
            &graphql,
            &format!(
                r#"{{ Goal(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ goal_id }} }}"#,
                escape_graphql_string(&thread_id)
            ),
        ))
        .await?;
    assert_eq!(
        cleared_rows
            .pointer("/data/Goal")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    Ok(())
}
