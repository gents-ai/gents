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
    let default_behavior_id = default_behavior_id_for_agent(&agent_did);

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

    // Seed an AgentRequest (so the agent's session resolves), an AgentSession
    // and AgentMessage for /sessions, plus a CompactionEntry so the
    // agent-scoped context budget has something to aggregate.
    for mutation in [
        format!(
            r#"mutation {{ create_AgentSession(input: {{ session_id: "self-budget-session", agent_name: "{}", behavior_id: "{}", started: "2026-06-02T09:59:00Z", status: "active" }}) {{ _docID }} }}"#,
            escape_graphql_string(&agent_name),
            escape_graphql_string(&default_behavior_id),
        ),
        format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "self-budget-req", agent_did: "{agent_did}", session_id: "self-budget-session", status: "completed", created_at: "2026-06-02T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        r#"mutation { create_AgentMessage(input: { message_key: "self-budget-session:1", session_id: "self-budget-session", sequence: 1, role: "user", content: "hello", timestamp: "2026-06-02T10:01:00Z" }) { _docID } }"#.to_string(),
        r#"mutation { create_CompactionEntry(input: { compaction_key: "self-budget-ce", session_id: "self-budget-session", sequence: 1, original_tokens: 1234, compacted_tokens: 567, created_at: "2026-06-02T10:00:00Z" }) { _docID } }"#.to_string(),
    ] {
        let seed = client
            .post(graphql.as_str())
            .json(&serde_json::json!({ "query": mutation }))
            .send()
            .await
            .context("seeding self-view fixtures")?;
        let seed_body: Value = seed.json().await.context("reading seed mutation response")?;
        assert!(
            seed_body.get("errors").is_none(),
            "seed mutation returned errors: {seed_body}"
        );
    }

    // /status carries the behavior join plus context budget/indicator.
    let status_response = client
        .get(format!("http://127.0.0.1:{port}/status"))
        .send()
        .await
        .context("fetching /status after seeding context fixtures")?;
    assert!(
        status_response.status().is_success(),
        "unexpected /status response: {status_response:?}"
    );
    let status: Value = status_response
        .json()
        .await
        .context("reading /status body")?;
    let behaviors = status
        .get("behaviors")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .unwrap_or_else(|| panic!("expected /status to include behaviors: {status}"));
    assert!(
        behaviors.iter().any(|behavior| {
            behavior.get("model_name").and_then(Value::as_str) == Some(model_name.as_str())
                // `/status` serializes behaviors as `SelfBehavior`, whose joined
                // backend URL field is `endpoint` (not `backend_endpoint`, which
                // is the field name in the separate `/behavior` detail view).
                && behavior
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .is_some_and(|endpoint| !endpoint.is_empty())
        }),
        "expected /status behavior joined with backend endpoint for model {model_name}: {status}"
    );
    let budget = status
        .get("context_budget")
        .unwrap_or_else(|| panic!("expected /status to include context_budget: {status}"));
    assert_eq!(
        budget.get("compaction_count").and_then(Value::as_i64),
        Some(1),
        "expected agent-scoped context_budget to count exactly the seeded compaction: {status}"
    );
    assert_eq!(
        budget.get("latest_original_tokens").and_then(Value::as_i64),
        Some(1234),
        "expected context_budget latest tokens from the seeded compaction: {status}"
    );
    let context = status
        .get("context")
        .unwrap_or_else(|| panic!("expected /status to include context indicator: {status}"));
    assert_eq!(
        context.get("compaction_count").and_then(Value::as_i64),
        Some(1),
        "expected /status context to mirror compaction count: {status}"
    );
    assert_eq!(
        context.get("current_estimate").and_then(Value::as_i64),
        Some(567),
        "expected /status context current_estimate from latest compacted tokens: {status}"
    );

    let sessions_response = client
        .get(format!("http://127.0.0.1:{port}/sessions?limit=1"))
        .send()
        .await
        .context("fetching /sessions")?;
    assert!(
        sessions_response.status().is_success(),
        "unexpected /sessions response: {sessions_response:?}"
    );
    let sessions: Value = sessions_response
        .json()
        .await
        .context("reading /sessions body")?;
    assert_eq!(
        sessions.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    let session = sessions
        .get("sessions")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .unwrap_or_else(|| panic!("expected /sessions to include the seeded row: {sessions}"));
    assert_eq!(
        session.get("session_id").and_then(Value::as_str),
        Some("self-budget-session")
    );
    assert_eq!(
        session.get("request_count").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        session.get("message_count").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        session.get("compaction_count").and_then(Value::as_i64),
        Some(1)
    );

    // /fleet reshapes per-agent_did runtime + request counts.
    let fleet_response = client
        .get(format!("http://127.0.0.1:{port}/fleet"))
        .send()
        .await
        .context("fetching /fleet")?;
    assert!(
        fleet_response.status().is_success(),
        "unexpected /fleet response: {fleet_response:?}"
    );
    let fleet: Value = fleet_response.json().await.context("reading /fleet body")?;
    assert!(
        fleet
            .get("agents")
            .and_then(Value::as_array)
            .is_some_and(|agents| agents.iter().any(|agent| {
                agent.get("agent_did").and_then(Value::as_str) == Some(agent_did.as_str())
                    && agent.get("process_state").and_then(Value::as_str) == Some("ready")
            })),
        "expected /fleet to list this agent in ready state: {fleet}"
    );

    // /mcp/pool joins registered MCP services with this agent's persisted
    // health state, including the last observed tool count.
    let escaped_agent_did = escape_graphql_string(&agent_did);
    for mutation in [
        r#"mutation {
            create_ToolServiceRegistry(input: {
                service_id: "runtime-mcp-pool-obs",
                display_name: "Runtime Observability",
                description: "Runtime endpoint fixture",
                hostname: "studio-1",
                tailscale_ip: "100.64.0.10",
                lan_ip: "192.168.1.10",
                mcp_port: 9201,
                mcp_path: "/mcp",
                send_agent_did: true,
                status: "online",
                version: "test",
                updated_at: "2026-06-05T00:00:00Z"
            }) { _docID }
        }"#
        .to_string(),
        format!(
            r#"mutation {{
                create_ToolServiceHealthState(input: {{
                    service_id: "runtime-mcp-pool-obs",
                    agent_did: "{escaped_agent_did}",
                    endpoint: "http://100.64.0.10:9201/mcp",
                    status: "healthy",
                    tool_count: 3,
                    failure_count: 0,
                    k_max: 3,
                    last_probe_at: "2026-06-05T00:00:00Z",
                    last_seen: "2026-06-05T00:00:00Z",
                    updated_at: "2026-06-05T00:00:00Z"
                }}) {{ _docID }}
            }}"#
        ),
    ] {
        graphql_query(&graphql, &mutation)
            .await
            .context("seeding MCP pool fixtures")?;
    }

    let mcp_pool_response = client
        .get(format!("http://127.0.0.1:{port}/mcp/pool"))
        .send()
        .await
        .context("fetching /mcp/pool")?;
    assert!(
        mcp_pool_response.status().is_success(),
        "unexpected /mcp/pool response: {mcp_pool_response:?}"
    );
    let mcp_pool: Value = mcp_pool_response
        .json()
        .await
        .context("reading /mcp/pool body")?;
    assert_eq!(
        mcp_pool.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        mcp_pool.pointer("/totals/online").and_then(Value::as_i64),
        Some(1),
        "expected /mcp/pool totals to count the seeded online service: {mcp_pool}"
    );
    assert_eq!(
        mcp_pool.pointer("/totals/healthy").and_then(Value::as_i64),
        Some(1),
        "expected /mcp/pool totals to count the seeded healthy service: {mcp_pool}"
    );
    assert!(
        mcp_pool
            .get("services")
            .and_then(Value::as_array)
            .is_some_and(|services| services.iter().any(|service| {
                service.get("service_id").and_then(Value::as_str) == Some("runtime-mcp-pool-obs")
                    && service.get("tool_count").and_then(Value::as_i64) == Some(3)
                    && service.get("health_status").and_then(Value::as_str) == Some("healthy")
            })),
        "expected /mcp/pool to include the seeded service and tool count: {mcp_pool}"
    );

    // /mcp is opt-in: this server was started without --enable-mcp, so the
    // endpoint must not be mounted.
    let mcp_off = client
        .get(format!("http://127.0.0.1:{port}/mcp"))
        .send()
        .await
        .context("probing /mcp")?;
    assert_eq!(
        mcp_off.status(),
        reqwest::StatusCode::NOT_FOUND,
        "expected /mcp to be absent without --enable-mcp"
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

    let client = reqwest::Client::new();
    let stable_deadline = Instant::now() + Duration::from_secs(5);
    let (
        snapshot,
        active_calls,
        expected_backend_running,
        expected_backend_queued,
        expected_behavior_running,
        expected_behavior_queued,
    ) = loop {
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
        let active_calls = active_inference_calls_for_backend(&graphql, &backend_id).await?;
        let backend_running = count_inference_calls(&active_calls, None, "running");
        let backend_queued = count_inference_calls(&active_calls, None, "queued");
        let snapshot_running = snapshot.pointer("/totals/assigned").and_then(Value::as_i64);
        let snapshot_queued = snapshot.pointer("/totals/queued").and_then(Value::as_i64);
        if snapshot_running == Some(backend_running) && snapshot_queued == Some(backend_queued) {
            break (
                snapshot,
                active_calls.clone(),
                backend_running,
                backend_queued,
                count_inference_calls(&active_calls, Some(&default_behavior_id), "running"),
                count_inference_calls(&active_calls, Some(&default_behavior_id), "queued"),
            );
        }
        if Instant::now() >= stable_deadline {
            return Err(anyhow!(
                "fleet slot snapshot did not stabilize with active inference calls; snapshot={snapshot}; active_calls={}",
                Value::Array(active_calls)
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let expected_available = 1_i64.saturating_sub(expected_backend_running);
    assert!(
        expected_backend_running >= 1,
        "test setup should hold at least one running call; active calls={}",
        Value::Array(active_calls)
    );

    assert_eq!(
        snapshot.pointer("/source").and_then(Value::as_str),
        Some("graphql.derived_admission_state")
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

/// Proves the `defra-agent query` command can reconstruct a full agent trace
/// (AgentRequest + AgentResponse + AgentMessage + AgentToolCall, stitched by
/// request_id / session_id) purely from structured query output — i.e. it can
/// retire Amygdala's hand-rolled GraphQL client / escaping / polling /
/// AgentMessage.content parsing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_command_reconstructs_a_trace() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-query-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-query-{}", Uuid::new_v4().simple());
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

    // Seed a linked trace (same request_id + session_id across collections).
    let client = reqwest::Client::new();
    let mutations = [
        format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "trace-req", agent_did: "{agent_did}", session_id: "trace-session", status: "completed", content: "hi", created_at: "2026-06-03T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        r#"mutation { create_AgentResponse(input: { response_key: "trace-resp", request_id: "trace-req", session_id: "trace-session", content: "hello", status: "completed", token_count: 7 }) { _docID } }"#.to_string(),
        r#"mutation { create_AgentMessage(input: { message_key: "trace-msg", session_id: "trace-session", sequence: 1, role: "assistant", content: "encoded-blob" }) { _docID } }"#.to_string(),
        r#"mutation { create_AgentToolCall(input: { tool_call_key: "trace-tc", request_id: "trace-req", session_id: "trace-session", tool_name: "defra_query", args: "{\"collection\":\"AgentRequest\"}", result: "{\"ok\":true}", status: "completed" }) { _docID } }"#.to_string(),
    ];
    for mutation in mutations {
        let resp = client
            .post(graphql.as_str())
            .json(&serde_json::json!({ "query": mutation }))
            .send()
            .await
            .context("seeding trace")?;
        let body: Value = resp.json().await?;
        assert!(
            body.get("errors").is_none(),
            "seed mutation errored: {body}"
        );
    }

    // Reconstruct each collection via `defra-agent query`.
    let request = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentRequest",
            "--field",
            "request_id",
            "--field",
            "session_id",
            "--field",
            "status",
            "--filter",
            r#"{"request_id":{"_eq":"trace-req"}}"#,
        ],
    )?;
    assert_eq!(
        request.get("count").and_then(Value::as_i64),
        Some(1),
        "{request}"
    );
    let req_row = &request["results"][0];
    assert_eq!(req_row["session_id"].as_str(), Some("trace-session"));
    assert_eq!(req_row["status"].as_str(), Some("completed"));

    let tool_calls = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentToolCall",
            "--field",
            "request_id",
            "--field",
            "tool_name",
            "--field",
            "args",
            "--field",
            "result",
            "--field",
            "status",
            "--filter",
            r#"{"request_id":{"_eq":"trace-req"}}"#,
        ],
    )?;
    assert_eq!(
        tool_calls.get("count").and_then(Value::as_i64),
        Some(1),
        "{tool_calls}"
    );
    let tc = &tool_calls["results"][0];
    assert_eq!(tc["tool_name"].as_str(), Some("defra_query"));
    // args/result are first-class JSON-string columns — no content reconstruction.
    let tc_args: Value = serde_json::from_str(tc["args"].as_str().unwrap())
        .context("tool call args parse as JSON")?;
    assert_eq!(tc_args["collection"].as_str(), Some("AgentRequest"));

    let responses = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentResponse",
            "--field",
            "request_id",
            "--field",
            "status",
            "--field",
            "token_count",
            "--filter",
            r#"{"request_id":{"_eq":"trace-req"}}"#,
        ],
    )?;
    let resp_row = &responses["results"][0];
    assert_eq!(resp_row["token_count"].as_i64(), Some(7));

    let messages = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentMessage",
            "--field",
            "session_id",
            "--field",
            "role",
            "--field",
            "sequence",
            "--filter",
            r#"{"session_id":{"_eq":"trace-session"}}"#,
        ],
    )?;
    let msg_row = &messages["results"][0];
    assert_eq!(msg_row["role"].as_str(), Some("assistant"));

    // Trace stitches across all four collections, entirely from structured output.
    assert_eq!(
        req_row["session_id"].as_str(),
        msg_row["session_id"].as_str()
    );
    assert_eq!(tc["request_id"].as_str(), resp_row["request_id"].as_str());

    // Secret guard holds on the CLI surface too.
    let denied = run_cli_failure_stderr(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "InferenceBackend",
            "--field",
            "api_key",
        ],
    )?;
    assert!(
        denied.contains("restricted"),
        "expected secret guard to fire: {denied}"
    );

    // Invalid field → agent-usable diagnostic with suggestions + inventory (#592).
    let diagnostic = run_cli_failure_stderr(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "AgentToolCall",
            "--field",
            "created_at",
        ],
    )?;
    assert!(diagnostic.contains("created_at"), "{diagnostic}");
    assert!(
        diagnostic.contains("started_at") && diagnostic.contains("completed_at"),
        "suggestions missing: {diagnostic}"
    );
    assert!(
        diagnostic.contains("tool_call_key"),
        "field inventory missing: {diagnostic}"
    );

    // Discovery mode: fields ["*"] returns the field inventory, secrets excluded.
    let inventory = run_cli_json(
        &home_dir,
        &[
            "query",
            "--graphql",
            &graphql,
            "--collection",
            "InferenceBackend",
            "--field",
            "*",
        ],
    )?;
    assert_eq!(inventory["discovery"], Value::Bool(true), "{inventory}");
    let field_names: Vec<&str> = inventory["fields"]
        .as_array()
        .context("discovery fields array")?
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(field_names.contains(&"backend_id"), "{field_names:?}");
    assert!(
        !field_names.contains(&"api_key") && !field_names.contains(&"api_key_env_var"),
        "secret leaked into discovery inventory: {field_names:?}"
    );

    Ok(())
}

/// Proves the `/mcp` endpoint serves `defra_query` to an external MCP client,
/// reconstructing trace data structurally (so an external consumer like
/// Amygdala can retire its hand-rolled stack) and still enforcing the secret
/// guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_endpoint_serves_defra_query() -> Result<()> {
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use rmcp::ServiceExt;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-mcp-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-mcp-{}", Uuid::new_v4().simple());
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

    // MCP is opt-in; the endpoint only mounts with --enable-mcp.
    let mut serve = spawn_server_with_env(&home_dir, port, &["--enable-mcp"], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Seed a linked request + tool call.
    let http = reqwest::Client::new();
    for mutation in [
        format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "mcp-req", agent_did: "{agent_did}", session_id: "mcp-session", status: "completed", created_at: "2026-06-03T10:00:00Z" }}) {{ _docID }} }}"#
        ),
        r#"mutation { create_AgentToolCall(input: { tool_call_key: "mcp-tc", request_id: "mcp-req", session_id: "mcp-session", tool_name: "defra_query", args: "{\"collection\":\"AgentRequest\"}", result: "{\"ok\":true}", status: "completed" }) { _docID } }"#.to_string(),
    ] {
        let resp = http
            .post(graphql.as_str())
            .json(&serde_json::json!({ "query": mutation }))
            .send()
            .await
            .context("seeding mcp trace")?;
        let body: Value = resp.json().await?;
        assert!(body.get("errors").is_none(), "seed mutation errored: {body}");
    }

    // Connect an MCP client to the mounted /mcp endpoint.
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("http://127.0.0.1:{port}/mcp"));
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
    let mcp = ().serve(transport).await.context("MCP client handshake with /mcp")?;

    // The server advertises the defra_query tool.
    let tools = mcp.peer().list_tools(None).await.context("list_tools")?;
    assert!(
        tools
            .tools
            .iter()
            .any(|tool| tool.name.as_ref() == "defra_query"),
        "expected defra_query in advertised tools: {:?}",
        tools
            .tools
            .iter()
            .map(|t| t.name.as_ref())
            .collect::<Vec<_>>()
    );

    // Reconstruct the tool call structurally via the MCP tool.
    let args = serde_json::json!({
        "collection": "AgentToolCall",
        "fields": ["request_id", "tool_name", "args", "result", "status"],
        "filter": { "request_id": { "_eq": "mcp-req" } }
    });
    let params =
        CallToolRequestParams::new("defra_query").with_arguments(args.as_object().unwrap().clone());
    let result = mcp
        .peer()
        .call_tool(params)
        .await
        .context("call_tool defra_query")?;
    let text = result
        .content
        .iter()
        .filter_map(|content| content.raw.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("");
    let payload: Value = serde_json::from_str(&text).context("MCP tool result is JSON")?;
    assert_eq!(payload["count"].as_i64(), Some(1), "{payload}");
    let tc = &payload["results"][0];
    assert_eq!(tc["tool_name"].as_str(), Some("defra_query"));
    assert_eq!(tc["request_id"].as_str(), Some("mcp-req"));

    // Secret guard holds over MCP too.
    let denied_args =
        serde_json::json!({ "collection": "InferenceBackend", "fields": ["api_key"] });
    let denied_params = CallToolRequestParams::new("defra_query")
        .with_arguments(denied_args.as_object().unwrap().clone());
    let denied = mcp.peer().call_tool(denied_params).await;
    let blocked = match denied {
        Err(_) => true,
        Ok(result) => {
            result.is_error == Some(true)
                || result.content.iter().any(|content| {
                    content
                        .raw
                        .as_text()
                        .map(|t| t.text.contains("restricted"))
                        .unwrap_or(false)
                })
        }
    };
    assert!(
        blocked,
        "expected MCP defra_query to block api_key selection"
    );

    let _ = mcp.cancel().await;
    Ok(())
}
