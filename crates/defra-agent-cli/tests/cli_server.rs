// Soft-cap justified: 5 server startup scenarios share setup (port allocation, runtime state, degraded-mode wiring); splitting would duplicate ~40 lines per file.
mod support;
use support::*;

use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use defra_agent::{default_behavior_id_for_agent, default_tool_selection_id_for_behavior};
use serde_json::Value;
use uuid::Uuid;

fn generated_backend_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:backend")
}

fn generated_tool_selection_id_for_agent(agent_did: &str) -> String {
    let default_behavior_id = default_behavior_id_for_agent(agent_did);
    default_tool_selection_id_for_behavior(&default_behavior_id)
}

fn find_snapshot_row<'a>(
    snapshot: &'a Value,
    collection: &str,
    key: &str,
    expected: &str,
) -> Result<&'a Value> {
    snapshot
        .get(collection)
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get(key).and_then(Value::as_str) == Some(expected))
        })
        .ok_or_else(|| anyhow!("missing {collection} row with {key}={expected}: {snapshot}"))
}

async fn wait_for_inference_call_state(
    graphql: &str,
    request_id: &str,
    expected_state: &str,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    InferenceCall(
                        filter: {{ request_id: {{ _eq: "{}" }} }},
                        order: {{ call_seq: ASC }}
                    ) {{
                        request_id
                        backend_id
                        behavior_id
                        call_state
                    }}
                }}"#,
                escape_graphql_string(request_id),
            ),
        )
        .await?;
        let rows = response
            .pointer("/data/InferenceCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows
            .iter()
            .find(|row| row.get("call_state").and_then(Value::as_str) == Some(expected_state))
        {
            return Ok(row.clone());
        }

        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for InferenceCall request_id={request_id} call_state={expected_state}; last rows={}",
                Value::Array(rows)
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn active_inference_calls_for_backend(graphql: &str, backend_id: &str) -> Result<Vec<Value>> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                InferenceCall(
                    filter: {{ backend_id: {{ _eq: "{}" }} }},
                    order: {{ call_seq: ASC }}
                ) {{
                    request_id
                    backend_id
                    behavior_id
                    call_state
                }}
            }}"#,
            escape_graphql_string(backend_id),
        ),
    )
    .await?;
    Ok(response
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            matches!(
                row.get("call_state").and_then(Value::as_str),
                Some("running" | "queued")
            )
        })
        .collect())
}

fn count_inference_calls(rows: &[Value], behavior_id: Option<&str>, call_state: &str) -> i64 {
    rows.iter()
        .filter(|row| {
            row.get("call_state").and_then(Value::as_str) == Some(call_state)
                && behavior_id.is_none_or(|expected| {
                    row.get("behavior_id").and_then(Value::as_str) == Some(expected)
                })
        })
        .count() as i64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_prometheus_metrics_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-metrics-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-metrics-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

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

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let client = reqwest::Client::new();

    let version_response = client
        .get(format!("http://127.0.0.1:{port}/version"))
        .send()
        .await
        .context("fetching /version")?;
    assert!(
        version_response.status().is_success(),
        "unexpected /version status: {version_response:?}"
    );
    let version: Value = version_response
        .json()
        .await
        .context("reading /version body")?;
    assert_eq!(
        version.get("service").and_then(Value::as_str),
        Some("defra-agent")
    );
    assert_eq!(
        version.get("binary").and_then(Value::as_str),
        Some("defra-agent")
    );
    assert_eq!(
        version.get("package").and_then(Value::as_str),
        Some("defra-agent-cli")
    );
    assert_eq!(
        version.get("version").and_then(Value::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        version.get("build").and_then(Value::as_object).is_some(),
        "expected build metadata in /version body: {version}"
    );

    let health_response = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .context("fetching /healthz")?;
    assert!(
        health_response.status().is_success(),
        "unexpected /healthz status: {health_response:?}"
    );
    let health: Value = health_response
        .json()
        .await
        .context("reading /healthz body")?;
    assert_eq!(health.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(health.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(
        health.get("service").and_then(Value::as_str),
        Some("defra-agent")
    );
    assert_eq!(
        health
            .pointer("/checks/runtime/ready")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        health
            .get("runtimes")
            .and_then(Value::as_array)
            .is_some_and(|runtimes| runtimes.iter().any(|runtime| {
                runtime.get("agent_did").and_then(Value::as_str) == Some(agent_did.as_str())
            })),
        "expected runtime row for {agent_did} in /healthz body: {health}"
    );

    let status_response = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    assert_eq!(
        status.get("agent_name").and_then(Value::as_str),
        Some(agent_name.as_str())
    );
    assert_eq!(
        status.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        status.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert!(
        status
            .get("p2p_listen_addresses")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()),
        "expected /status to include P2P listen addresses: {status}"
    );
    assert!(
        status
            .pointer("/liveness/active_native_executors")
            .and_then(Value::as_array)
            .is_some(),
        "expected /status liveness to include active_native_executors: {status}"
    );
    assert_eq!(
        status
            .pointer("/liveness/active_native_executors_available")
            .and_then(Value::as_bool),
        Some(true)
    );

    let response = client
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .context("fetching /metrics")?;
    assert!(
        response.status().is_success(),
        "unexpected status: {response:?}"
    );
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type: {content_type}"
    );
    let body = response.text().await.context("reading /metrics body")?;
    assert!(
        body.contains("# HELP defra_agent_up"),
        "expected defra_agent_up help text in metrics body:\n{body}"
    );
    assert!(
        body.contains(r#"defra_agent_up 1"#),
        "expected defra_agent_up sample in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"defra_agent_runtime_process_state{{agent_did="{agent_did}",state="ready"}} 1"#
        )),
        "expected ready process-state metric in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"defra_agent_runtime_active_generation{{agent_did="{agent_did}"}}"#
        )),
        "expected active-generation metric in metrics body:\n{body}"
    );
    assert!(
        body.contains("defra_agent_backend_enabled"),
        "expected backend metrics in metrics body:\n{body}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_fleet_slot_snapshot_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-fleet-slots-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-fleet-slots-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("fleet slots live request {}", Uuid::new_v4());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--max-concurrent",
            "1",
            "--max-queue-depth",
            "2",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let default_behavior_id = init
        .pointer("/init/default_behavior_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(&agent_did));

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &request_content,
            "--no-wait",
        ],
    )?;
    let request_id = submitted
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("request submit output missing request_id: {submitted}"))?
        .to_string();
    wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["processing"],
        Duration::from_secs(30),
    )
    .await?;
    wait_for_inference_call_state(&graphql, &request_id, "running").await?;
    let active_calls = active_inference_calls_for_backend(&graphql, &backend_id).await?;
    let expected_backend_running = count_inference_calls(&active_calls, None, "running");
    let expected_backend_queued = count_inference_calls(&active_calls, None, "queued");
    let expected_behavior_running =
        count_inference_calls(&active_calls, Some(&default_behavior_id), "running");
    let expected_behavior_queued =
        count_inference_calls(&active_calls, Some(&default_behavior_id), "queued");
    let expected_available = 1_i64.saturating_sub(expected_backend_running);
    assert!(
        expected_backend_running >= 1,
        "test setup should hold at least one running call; active calls={}",
        Value::Array(active_calls.clone())
    );

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://127.0.0.1:{port}/fleet/slots"))
        .send()
        .await
        .context("fetching /fleet/slots")?;
    assert!(
        response.status().is_success(),
        "unexpected /fleet/slots response: {response:?}"
    );
    let snapshot: Value = response.json().await.context("reading /fleet/slots body")?;

    assert_eq!(
        snapshot.pointer("/source").and_then(Value::as_str),
        Some("graphql.derived_inference_call_rows")
    );
    assert_eq!(
        snapshot.pointer("/totals/assigned").and_then(Value::as_i64),
        Some(expected_backend_running)
    );
    assert_eq!(
        snapshot
            .pointer("/totals/available")
            .and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(
        snapshot.pointer("/totals/max").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        snapshot.pointer("/totals/queued").and_then(Value::as_i64),
        Some(expected_backend_queued)
    );
    assert_eq!(
        snapshot
            .pointer("/expired/processing_requests")
            .and_then(Value::as_i64),
        Some(0)
    );

    let backend = find_snapshot_row(&snapshot, "backends", "backend_id", &backend_id)?;
    assert_eq!(
        backend.get("running").and_then(Value::as_i64),
        Some(expected_backend_running)
    );
    assert_eq!(
        backend.get("queued").and_then(Value::as_i64),
        Some(expected_backend_queued)
    );
    assert_eq!(
        backend.get("available").and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(
        backend.get("max_concurrent").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        backend.get("max_queue_depth").and_then(Value::as_i64),
        Some(2)
    );
    assert_eq!(
        backend.get("accepting_admission").and_then(Value::as_bool),
        Some(true)
    );

    let behavior = find_snapshot_row(&snapshot, "behaviors", "behavior_id", &default_behavior_id)?;
    assert_eq!(
        behavior.get("backend_id").and_then(Value::as_str),
        Some(backend_id.as_str())
    );
    assert_eq!(
        behavior.get("assigned").and_then(Value::as_i64),
        Some(expected_behavior_running)
    );
    assert_eq!(
        behavior.get("available").and_then(Value::as_i64),
        Some(expected_available)
    );
    assert_eq!(behavior.get("max").and_then(Value::as_i64), Some(1));
    assert_eq!(
        behavior.get("queued").and_then(Value::as_i64),
        Some(expected_behavior_queued)
    );

    let cli_snapshot = run_cli_json(&home_dir, &["fleet", "slots", "--graphql", &graphql])?;
    assert_eq!(
        cli_snapshot.pointer("/totals/assigned"),
        snapshot.pointer("/totals/assigned")
    );
    assert_eq!(
        cli_snapshot.pointer("/backends/0/backend_id"),
        snapshot.pointer("/backends/0/backend_id")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_real_initialized_did_without_key_path() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".defra-agent");
    fs::create_dir_all(&agent_home)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "agent_name": "mini-1-steward",
            "agent_did": agent_did,
            "key_path": null,
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("no key_path or identity_backend"),
        "expected no-key-path/backend error, got:\n{stderr}"
    );
    assert!(
        !agent_home.join("keys").exists(),
        "server must not create a fallback file-key identity for a no-key initialized home"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_macos_keychain_identity_without_label() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".defra-agent");
    fs::create_dir_all(&agent_home)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "agent_name": "mini-1-steward",
            "agent_did": agent_did,
            "key_path": null,
            "identity_backend": "macos-keychain",
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("macos-keychain"),
        "expected macos-keychain error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("no keychain_label"),
        "expected missing keychain label error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_rejects_real_initialized_did_with_missing_key_file_without_creating_it(
) -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let agent_home = home_env.join(".defra-agent");
    let key_path = agent_home.join("keys").join("missing.key");
    fs::create_dir_all(&agent_home)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &agent_home.join("init.json"),
        &serde_json::json!({
            "home": agent_home.to_string_lossy(),
            "agent_name": "mini-1-steward",
            "agent_did": agent_did,
            "key_path": key_path.to_string_lossy(),
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;

    let port = allocate_port()?;
    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "server",
            "--home",
            agent_home.to_str().expect("utf-8 home"),
            "--http-port",
            &port.to_string(),
        ],
    )?;
    assert!(
        stderr.contains("requires identity key"),
        "expected missing-key error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("to already exist"),
        "expected no-create hint, got:\n{stderr}"
    );
    assert!(
        !key_path.exists(),
        "server must not create a new key for a real initialized DID with missing key file"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_startup_with_iroh_p2p_reports_runtime_connectivity() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-p2p-ready-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-p2p-ready-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

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
    let default_behavior_id = default_behavior_id_for_agent(&agent_did);
    let (mut serve, readiness) = spawn_server_with_ready_json(
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

    assert_eq!(
        readiness.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );
    assert_eq!(
        readiness.get("default_behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(
        readiness.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(readiness
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let status_response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    assert_eq!(
        status.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        status.get("agent_name").and_then(Value::as_str),
        Some(agent_name.as_str())
    );
    assert_eq!(
        status.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(status
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let runtime_state = read_runtime_state_json(&home_dir)?;
    assert_eq!(
        runtime_state.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        runtime_state.get("p2p_peer_id"),
        readiness.get("p2p_peer_id")
    );
    assert_eq!(
        runtime_state.get("p2p_listen_addresses"),
        readiness.get("p2p_listen_addresses")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_startup_defaults_to_iroh_p2p_for_desktop_pairing() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-default-iroh-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-default-iroh-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

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
    let (mut serve, readiness) = spawn_server_with_ready_json(&home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_eq!(
        readiness.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert!(readiness
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty()));
    assert!(readiness
        .get("p2p_listen_addresses")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()));

    let runtime_state = read_runtime_state_json(&home_dir)?;
    assert_eq!(
        runtime_state.get("p2p_transport").and_then(Value::as_str),
        Some("iroh")
    );
    assert_eq!(
        runtime_state.get("p2p_peer_id"),
        readiness.get("p2p_peer_id")
    );
    assert_eq!(
        runtime_state.get("p2p_listen_addresses"),
        readiness.get("p2p_listen_addresses")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_starts_in_degraded_mode_when_backend_is_unavailable() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-degraded-model-{}", Uuid::new_v4().simple());
    let warm_port = allocate_port()?;
    let port = allocate_port()?;
    let agent_name = format!("cli-degraded-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "http://127.0.0.1:9/v1",
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();

    let mut warm_server = spawn_server(&home_dir, warm_port)?;
    wait_for_port(warm_port, &mut warm_server)?;
    wait_for_runtime_ready(&graphql_url(warm_port), &agent_did, Duration::from_secs(30)).await?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "set",
            "--graphql",
            &graphql_url(warm_port),
            "--backend-id",
            &backend_id,
            "--name",
            &backend_id,
            "--provider-kind",
            "OpenAiCompatible",
            "--endpoint",
            "http://127.0.0.1:9/v1",
            "--max-concurrent",
            "1",
            "--probe-status",
            "unknown",
        ],
    )?;
    warm_server
        .child
        .kill()
        .context("stopping warm server after backend downgrade")?;
    warm_server
        .child
        .wait()
        .context("waiting for warm server shutdown")?;

    let (mut serve, readiness) = spawn_server_with_ready_json(&home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_eq!(
        readiness.get("status").and_then(Value::as_str),
        Some("serving")
    );
    assert_eq!(
        readiness.get("behavior_readiness").and_then(Value::as_str),
        Some("degraded")
    );
    assert_eq!(
        readiness
            .get("runnable_behaviors")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    let unavailable = readiness
        .get("unavailable_behaviors")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("readiness missing unavailable_behaviors: {readiness}"))?;
    assert_eq!(unavailable.len(), 1);
    let reason = unavailable
        .values()
        .next()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        reason.contains("probe_status=unknown"),
        "unexpected unavailable reason: {reason}"
    );
    assert_eq!(
        readiness.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    let status = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        status.get("process_state").and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        status.get("behavior_readiness").and_then(Value::as_str),
        Some("degraded")
    );
    assert_eq!(
        status
            .get("runnable_behavior_count")
            .and_then(Value::as_i64),
        Some(0)
    );
    let status_unavailable = status
        .get("unavailable_behaviors")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("status output missing unavailable_behaviors: {status}"))?;
    assert_eq!(status_unavailable.len(), 1);
    let status_reason = status_unavailable
        .values()
        .next()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        status_reason.contains("probe_status=unknown"),
        "unexpected status unavailable reason: {status_reason}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_and_server_use_backend_specific_api_key_env_var() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-auth-model-{}", Uuid::new_v4().simple());
    let expected_reply = "AUTH_BACKEND_OK";
    let mock_endpoint = MockChatEndpoint::start_with_required_bearer(
        &model_name,
        expected_reply,
        Some("backend-key"),
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-auth-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--api-key-env-var",
            "DEFRA_AGENT_TEST_CLI_BACKEND_KEY",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/api_key_env_var")
            .and_then(Value::as_str),
        Some("DEFRA_AGENT_TEST_CLI_BACKEND_KEY")
    );
    let agent_did = agent_did_from_init(&init)?;
    let backend_id = generated_backend_id_for_agent(&agent_did);
    let tool_selection_id = generated_tool_selection_id_for_agent(&agent_did);

    let mut serve = spawn_server_with_env(
        &home_dir,
        port,
        &[],
        &[("DEFRA_AGENT_TEST_CLI_BACKEND_KEY", "backend-key")],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        Some("DEFRA_AGENT_TEST_CLI_BACKEND_KEY"),
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    let output = run_cli_text(
        &home_dir,
        &[
            "chat",
            "backend auth should flow through the configured env var",
        ],
    )?;
    assert!(
        output.contains(expected_reply),
        "expected chat output to contain {expected_reply}, got:\n{output}"
    );

    Ok(())
}
