use crate::support::*;

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
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

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
    assert_eq!(
        output
            .pointer("/p2p_admission/max_pending_dags")
            .and_then(Value::as_u64),
        Some(p2p::sync::DEFAULT_MAX_PENDING_DAGS as u64)
    );
    assert_eq!(
        output
            .pointer("/p2p_admission/max_concurrent_push_tasks")
            .and_then(Value::as_u64),
        Some(p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS as u64)
    );
    assert_eq!(
        output
            .pointer("/p2p/p2p_admission/max_pending_dags")
            .and_then(Value::as_u64),
        Some(p2p::sync::DEFAULT_MAX_PENDING_DAGS as u64)
    );

    let metrics = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .context("GET /metrics")?
        .error_for_status()
        .context("/metrics status")?
        .text()
        .await
        .context("/metrics body")?;
    assert!(
        metrics.contains("gents_p2p_enabled 1"),
        "metrics missing p2p_enabled: {metrics}"
    );
    assert!(
        metrics.contains(&format!(
            "gents_p2p_admission_max_pending_dags {}",
            p2p::sync::DEFAULT_MAX_PENDING_DAGS
        )),
        "metrics missing pending-dag admission: {metrics}"
    );
    assert!(
        metrics.contains(&format!(
            "gents_p2p_admission_max_concurrent_push_tasks {}",
            p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS
        )),
        "metrics missing push-task admission: {metrics}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_and_metrics_surface_overridden_p2p_admission_knobs() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-p2p-admission-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-p2p-admission-{}", Uuid::new_v4().simple());
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
    let (mut serve, ready) = spawn_server_with_ready_json(
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
            "--p2p-max-pending-dags",
            "17",
            "--p2p-max-concurrent-push-tasks",
            "3",
            "--p2p-max-concurrent-dag-fetches",
            "5",
            "--p2p-rate-limit-burst",
            "111",
            "--p2p-rate-limit-rate",
            "22.5",
        ],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_eq!(
        ready
            .pointer("/p2p_admission/max_pending_dags")
            .and_then(Value::as_u64),
        Some(17),
        "ready JSON: {ready}"
    );
    assert_eq!(
        ready
            .pointer("/p2p_admission/max_concurrent_push_tasks")
            .and_then(Value::as_u64),
        Some(3),
        "ready JSON: {ready}"
    );

    let output = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        output
            .pointer("/p2p_admission/max_pending_dags")
            .and_then(Value::as_u64),
        Some(17),
        "status: {output}"
    );
    assert_eq!(
        output
            .pointer("/p2p_admission/max_concurrent_push_tasks")
            .and_then(Value::as_u64),
        Some(3),
        "status: {output}"
    );
    assert_eq!(
        output
            .pointer("/p2p_admission/max_concurrent_dag_fetches")
            .and_then(Value::as_u64),
        Some(5),
        "status: {output}"
    );
    assert_eq!(
        output
            .pointer("/p2p_admission/rate_limit_burst")
            .and_then(Value::as_u64),
        Some(111),
        "status: {output}"
    );
    assert_eq!(
        output
            .pointer("/p2p_admission/rate_limit_rate")
            .and_then(Value::as_f64),
        Some(22.5),
        "status: {output}"
    );

    let metrics = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .context("GET /metrics")?
        .error_for_status()
        .context("/metrics status")?
        .text()
        .await
        .context("/metrics body")?;
    for needle in [
        "gents_p2p_admission_max_pending_dags 17",
        "gents_p2p_admission_max_concurrent_push_tasks 3",
        "gents_p2p_admission_max_concurrent_dag_fetches 5",
        "gents_p2p_admission_rate_limit_burst 111",
        "gents_p2p_admission_rate_limit_rate 22.5",
    ] {
        assert!(
            metrics.contains(needle),
            "metrics missing `{needle}`: {metrics}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_liveness_surfaces_expired_processing_request_and_running_tool() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-liveness-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-liveness-{}", Uuid::new_v4().simple());
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

    let stuck_request_id = format!("stuck-req-{}", Uuid::new_v4().simple());
    let stuck_session_id = format!("stuck-session-{}", Uuid::new_v4().simple());
    let stuck_tool_call_key = format!("stuck-tc-{}", Uuid::new_v4().simple());
    let stuck_tool_call_id = format!("toolcall-{}", Uuid::new_v4().simple());

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "default",
                    session_id: "{session_id}",
                    content: "stuck request seeded for liveness surface test",
                    status: "processing",
                    lifecycle_state: "processing",
                    backend_id: "studios-cluster",
                    created_at: "2024-01-01T11:00:00Z",
                    claimed_at: "2024-01-01T11:00:01Z",
                    deadline: "2024-01-01T11:00:30Z",
                    retry_count: 0
                }}) {{ _docID }}
            }}"#,
            request_id = stuck_request_id,
            session_id = stuck_session_id,
        ),
    )
    .await?;

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{key}",
                    agent_did: "{agent_did}",
                    request_id: "{request_id}",
                    session_id: "{session_id}",
                    message_sequence: 1,
                    tool_name: "glob",
                    tool_call_id: "{tool_call_id}",
                    args: "{{}}",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "2024-01-01T11:00:05Z",
                    deadline_at: "2024-01-01T11:00:30Z"
                }}) {{ _docID }}
            }}"#,
            key = stuck_tool_call_key,
            request_id = stuck_request_id,
            session_id = stuck_session_id,
            tool_call_id = stuck_tool_call_id,
        ),
    )
    .await?;

    let output = run_cli_json(&home_dir, &["status"])?;
    let liveness = output
        .get("liveness")
        .expect("status output must include liveness block");
    assert!(
        liveness
            .get("active_native_executors")
            .and_then(Value::as_array)
            .is_some(),
        "CLI status liveness must include active_native_executors from live HTTP status when available: {liveness}"
    );
    assert_eq!(
        liveness
            .get("active_native_executors_available")
            .and_then(Value::as_bool),
        Some(true),
        "CLI status must distinguish live HTTP executor visibility from GraphQL-only liveness: {liveness}"
    );
    let expired = liveness
        .get("expired_processing_count")
        .and_then(Value::as_i64)
        .expect("liveness must expose expired_processing_count as i64");
    assert!(
        expired >= 1,
        "expired_processing_count must reflect the seeded stuck request, got {expired} in {liveness}"
    );

    let active_ids = liveness
        .get("active_request_ids")
        .and_then(Value::as_array)
        .expect("liveness must expose active_request_ids array");
    assert!(
        active_ids
            .iter()
            .any(|id| id.as_str() == Some(stuck_request_id.as_str())),
        "active_request_ids must contain seeded {stuck_request_id}, got {active_ids:?}"
    );

    let tool_calls = liveness
        .get("active_tool_calls")
        .and_then(Value::as_array)
        .expect("liveness must expose active_tool_calls array");
    let seeded_tool = tool_calls
        .iter()
        .find(|tc| tc.get("request_id").and_then(Value::as_str) == Some(stuck_request_id.as_str()))
        .unwrap_or_else(|| {
            panic!("active_tool_calls must contain a row for {stuck_request_id}: {tool_calls:?}")
        });
    assert_eq!(
        seeded_tool.get("tool_name").and_then(Value::as_str),
        Some("glob")
    );
    let running_age_ms = seeded_tool
        .get("running_age_ms")
        .and_then(Value::as_i64)
        .expect("active tool call must report running_age_ms");
    assert!(
        running_age_ms > 0,
        "running_age_ms must be positive for a row with started_at in the past, got {running_age_ms}"
    );
    assert_eq!(
        seeded_tool.get("deadline_expired").and_then(Value::as_bool),
        Some(true)
    );

    let request_view = liveness
        .get("requests")
        .and_then(Value::as_array)
        .and_then(|requests| {
            requests.iter().find(|r| {
                r.get("request_id").and_then(Value::as_str) == Some(stuck_request_id.as_str())
            })
        })
        .expect("requests array must include seeded stuck request");
    let last_progress_age_ms = request_view
        .get("last_progress_age_ms")
        .and_then(Value::as_i64)
        .expect("request view must expose last_progress_age_ms");
    assert!(
        last_progress_age_ms > 0,
        "last_progress_age_ms must be positive for stale request, got {last_progress_age_ms}"
    );
    assert_eq!(
        request_view
            .get("deadline_expired")
            .and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}
