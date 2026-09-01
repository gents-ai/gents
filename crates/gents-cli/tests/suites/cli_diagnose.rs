use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnose_works_from_local_home_without_server() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diagnose-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diagnose-{}", Uuid::new_v4().simple());

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

    let output = run_cli_json(&home_dir, &["diagnose"])?;
    assert_eq!(output.get("status").and_then(Value::as_str), Some("ok"));
    let runtime_schema = output
        .pointer("/checks/schemas")
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks.iter().find(|check| {
                check.get("collection").and_then(Value::as_str) == Some("AgentRuntime")
            })
        })
        .ok_or_else(|| anyhow!("diagnose output missing AgentRuntime schema check: {output}"))?;
    assert_eq!(
        runtime_schema
            .get("required_for_config")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        runtime_schema.get("ok").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output.get("access_mode").and_then(Value::as_str),
        Some("local")
    );
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output.get("graphql_reachable").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        output
            .pointer("/checks/default_behavior/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .pointer("/checks/tool_ceiling/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .pointer("/checks/runtime_behavior_readiness/required")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        output
            .pointer("/checks/runtime_behavior_readiness/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .pointer("/checks/backends/0/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        output.pointer("/checks/chatgpt_auth/ok").is_some(),
        "diagnose output missing ChatGPT auth check: {output}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_diagnose_uses_authoritative_readiness_and_rejects_malformed_rows() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-live-diagnose-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "live-diagnose",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server_with_env(&home_dir, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Keep the live assertions on the live access path. Auto-discovery intentionally
    // falls back to local storage when its endpoint probe fails, but the server owns
    // that store exclusively for the duration of this phase.
    let healthy = run_cli_json(&home_dir, &["diagnose", "--graphql", &graphql])?;
    assert_eq!(
        healthy
            .pointer("/checks/runtime_behavior_readiness/required")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        healthy
            .pointer("/checks/runtime_behavior_readiness/ok")
            .and_then(Value::as_bool),
        Some(true)
    );

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_AgentBehaviorReadiness(
                    filter: {{ agent_did: {{ _eq: "{}" }} }},
                    input: {{ snapshot_json: "{{}}" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    let malformed = run_cli_json(&home_dir, &["diagnose", "--graphql", &graphql])?;
    assert_eq!(
        malformed.get("status").and_then(Value::as_str),
        Some("degraded")
    );
    assert_eq!(
        malformed
            .pointer("/checks/runtime_behavior_readiness/ok")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        malformed
            .pointer("/runtime_behavior_readiness/state")
            .and_then(Value::as_str),
        Some("unknown")
    );

    let default_behavior_id = gents::default_behavior_id_for_agent(&agent_did);
    let stale_degraded_snapshot = serde_json::json!({
        "format_version": 1,
        "process_state": "ready",
        "active_generation": 1,
        "router_generation": 1,
        "default_behavior_id": default_behavior_id,
        "behaviors": [{
            "behavior_id": default_behavior_id,
            "state": "unavailable",
            "reason": "backend_disabled",
        }],
    });
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_AgentBehaviorReadiness(
                    filter: {{ agent_did: {{ _eq: "{}" }} }},
                    input: {{ snapshot_json: "{}" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&agent_did),
            escape_graphql_string(&serde_json::to_string(&stale_degraded_snapshot)?),
        ),
    )
    .await?;
    drop(serve);

    let offline = run_cli_json(&home_dir, &["diagnose"])?;
    assert_eq!(offline.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(
        offline
            .pointer("/checks/runtime_behavior_readiness/required")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        offline
            .pointer("/checks/runtime_behavior_readiness/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        offline
            .pointer("/checks/runtime_behavior_readiness/status")
            .and_then(Value::as_str),
        Some("degraded")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnose_with_explicit_graphql_does_not_reuse_unrelated_local_p2p_state() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-p2p-diagnose-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-p2p-diagnose-{}", Uuid::new_v4().simple());
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
    let mut serve = spawn_server_with_env(
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

    let output = run_cli_json(
        &home_dir,
        &["diagnose", "--graphql", "http://127.0.0.1:1/api/v0/graphql"],
    )?;
    assert_eq!(
        output.get("p2p_transport").and_then(Value::as_str),
        Some("none")
    );
    assert!(output.get("p2p_peer_id").is_none_or(Value::is_null));
    assert_eq!(
        output
            .pointer("/checks/p2p/transport")
            .and_then(Value::as_str),
        Some("none")
    );

    Ok(())
}
