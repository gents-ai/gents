mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reads_local_runtime_context_by_default() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-status-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-status-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output
            .pointer("/runtime/process_state")
            .and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        output.get("process_state").and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        output.get("reconcile_phase").and_then(Value::as_str),
        Some("idle")
    );
    assert_eq!(
        output
            .get("runnable_behavior_count")
            .and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        output.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_includes_p2p_runtime_info() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-p2p-status-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-p2p-status-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let (mut serve, _) = spawn_server_with_ready_json(
        &home_dir,
        port,
        &[
            "--p2p-bind-addr",
            "127.0.0.1",
            "--p2p-port",
            "0",
            "--p2p-relay-mode",
            "disabled",
            "--p2p-discovery",
            "disabled",
        ],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        output.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        output.pointer("/p2p/p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        output.get("p2p_enabled").and_then(Value::as_bool),
        Some(true)
    );
    assert!(output
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(output
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));
    assert!(output
        .get("p2p_shareable_address")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(output
        .pointer("/p2p/p2p_shareable_address")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));

    Ok(())
}
