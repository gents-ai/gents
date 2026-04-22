mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// End-to-end test for `defra-agent config task run --task-id --args`.
///
/// Seeds a `Task` + `AgentBehavior` via the standard apply path against a
/// running agent, then invokes the `config task run` subcommand and verifies
/// that it produces an `AgentRequest` with:
///   * `caused_by_trigger_kind = "manual"`,
///   * `caused_by_trigger_id = null`,
///   * `execution_origin = "interactive"`,
///   * `lifecycle_state = "pending"`,
///   * the rendered content from `prompt_template` after binding `args.*`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_task_run_creates_manual_agent_request() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-task-run-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-task-run-{}", Uuid::new_v4().simple());

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

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    write_manifest_root_from_export(&root, &exported)?;

    let behavior_id = exported
        .pointer("/agent_principal/default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing default behavior id"))?
        .to_string();
    let agent_did = exported
        .pointer("/agent_principal/agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing agent_did"))?
        .to_string();

    let task_id = format!("greet-{}", Uuid::new_v4().simple());
    let task_path = root.join("tasks").join("greet.json");
    write_json_file(
        &task_path,
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Greet",
            "description": "Manual-only task for CLI task run.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "hi {{ args.name }}",
            "enabled": true,
        }),
    )?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let applied = run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        applied.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );

    // Fire the manual run via the new CLI subcommand.
    let fire = run_cli_json(
        &home_dir,
        &[
            "config",
            "task",
            "run",
            "--task-id",
            &task_id,
            "--args",
            r#"{"name":"Amy"}"#,
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(fire.get("task_id").and_then(Value::as_str), Some(task_id.as_str()));
    assert_eq!(fire.get("status").and_then(Value::as_str), Some("pending"));
    assert_eq!(
        fire.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id.as_str())
    );
    assert_eq!(
        fire.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    let request_id = fire
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("fire output missing request_id: {fire}"))?
        .to_string();
    let request_doc_id = fire
        .get("request_doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("fire output missing request_doc_id: {fire}"))?;
    assert!(!request_doc_id.is_empty());

    // Verify lineage + rendered content on the AgentRequest row. We filter
    // by request_id so we don't race any other requests that may have been
    // written in parallel.
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    agent_did
                    behavior_id
                    content
                    status
                    lifecycle_state
                    execution_origin
                    caused_by_trigger_id
                    caused_by_trigger_kind
                }}
            }}"#,
            escape_graphql_string(&request_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "AgentRequest")?;
    assert_eq!(row.get("agent_did").and_then(Value::as_str), Some(agent_did.as_str()));
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id.as_str())
    );
    assert_eq!(row.get("content").and_then(Value::as_str), Some("hi Amy"));
    assert_eq!(row.get("status").and_then(Value::as_str), Some("pending"));
    assert_eq!(
        row.get("lifecycle_state").and_then(Value::as_str),
        Some("pending")
    );
    assert_eq!(
        row.get("execution_origin").and_then(Value::as_str),
        Some("interactive")
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(Value::as_str),
        Some("manual")
    );
    assert!(
        row.get("caused_by_trigger_id").is_some_and(Value::is_null),
        "caused_by_trigger_id must be null for manual runs, got {:?}",
        row.get("caused_by_trigger_id")
    );

    Ok(())
}

/// Running against a disabled Task must fail with a clear error and not
/// create an AgentRequest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_task_run_rejects_disabled_task() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-task-run-disabled-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-task-run-disabled-{}", Uuid::new_v4().simple());

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

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    write_manifest_root_from_export(&root, &exported)?;

    let behavior_id = exported
        .pointer("/agent_principal/default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing default behavior id"))?
        .to_string();
    let agent_did = exported
        .pointer("/agent_principal/agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing agent_did"))?
        .to_string();

    let task_id = format!("disabled-{}", Uuid::new_v4().simple());
    let task_path = root.join("tasks").join("disabled.json");
    write_json_file(
        &task_path,
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Disabled",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "noop",
            "enabled": false,
        }),
    )?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;

    let stderr = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "task",
            "run",
            "--task-id",
            &task_id,
            "--graphql",
            &graphql,
        ],
    )?;
    assert!(
        stderr.contains("disabled"),
        "expected disabled-task error on stderr, got: {stderr}"
    );

    // No AgentRequest should have been written for this task.
    let response = graphql_query(
        &graphql,
        r#"{ AgentRequest(filter: { caused_by_trigger_kind: { _eq: "manual" } }, limit: 5) { request_id } }"#,
    )
    .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "no manual AgentRequest rows expected, got {rows:?}"
    );

    Ok(())
}
