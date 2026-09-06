use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[path = "../../../gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_task_run_matches_lean_manual_dispatch_contract() -> Result<()> {
    let lean_case = lean_manual_dispatch_case()?;
    assert_eq!(lean_case.name, "manual_unconditional");
    assert_eq!(lean_case.trigger_id, None);
    assert_eq!(lean_case.trigger_kind, "manual");
    assert_eq!(lean_case.concurrency, "parallel");
    assert_eq!(lean_case.expected_result, "fired");
    assert_eq!(lean_case.expected_materialize_trigger_id, None);
    assert_eq!(
        lean_case.expected_materialize_trigger_kind.as_deref(),
        Some("manual")
    );
    assert_eq!(
        lean_case.expected_execution_origin.as_deref(),
        Some("interactive")
    );
    assert_eq!(
        lean_case.request_count_after,
        lean_case.request_count_before + 1
    );

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
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent_principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing default_behavior_id"))?
        .to_string();
    let agent_did = principal
        .get("agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing agent_did"))?
        .to_string();

    let task_id = format!("greet-{}", Uuid::new_v4().simple());
    let task_path = root
        .join("tasks")
        .join(crate::support::document_handle(&task_id))
        .join("object.json");
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

    let list = run_cli_json(&home_dir, &["task", "list", "--graphql", &graphql])?;
    let listed_task = list
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| {
            tasks
                .iter()
                .find(|task| task.get("task_id").and_then(Value::as_str) == Some(task_id.as_str()))
        })
        .ok_or_else(|| anyhow!("task list output missing {task_id}: {list}"))?;
    assert_eq!(
        listed_task.get("runnable").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        listed_task
            .pointer("/behavior/behavior_id")
            .and_then(Value::as_str),
        Some(behavior_id.as_str())
    );

    let shown = run_cli_json(
        &home_dir,
        &["task", "show", &task_id, "--graphql", &graphql],
    )?;
    assert_eq!(
        shown.pointer("/task/task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(shown.get("runnable").and_then(Value::as_bool), Some(true));
    assert_eq!(
        shown.pointer("/behavior/agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );

    let fire = run_cli_json(
        &home_dir,
        &[
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
    assert_eq!(
        fire.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
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

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    agent_did
                    behavior_id
                    content
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
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id.as_str())
    );
    assert_eq!(row.get("content").and_then(Value::as_str), Some("hi Amy"));
    let persisted_lifecycle_state = row.get("lifecycle_state").and_then(Value::as_str);
    assert!(
        matches!(
            persisted_lifecycle_state,
            Some("pending" | "claimed" | "processing" | "completed")
        ),
        "manual task run should create a request that is pending or has advanced through the daemon lifecycle, got lifecycle_state={persisted_lifecycle_state:?}"
    );
    assert_eq!(
        row.get("execution_origin").and_then(Value::as_str),
        lean_case.expected_execution_origin.as_deref()
    );
    assert_eq!(
        row.get("caused_by_trigger_kind").and_then(Value::as_str),
        lean_case.expected_materialize_trigger_kind.as_deref()
    );
    match lean_case.expected_materialize_trigger_id.as_deref() {
        Some(expected_id) => assert_eq!(
            row.get("caused_by_trigger_id").and_then(Value::as_str),
            Some(expected_id)
        ),
        None => assert!(
            row.get("caused_by_trigger_id").is_some_and(Value::is_null),
            "caused_by_trigger_id must be null for manual runs, got {:?}",
            row.get("caused_by_trigger_id")
        ),
    }

    Ok(())
}

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
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent_principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing default_behavior_id"))?
        .to_string();
    let agent_did = principal
        .get("agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing agent_did"))?
        .to_string();

    let task_id = format!("disabled-{}", Uuid::new_v4().simple());
    let task_path = root
        .join("tasks")
        .join(crate::support::document_handle(&task_id))
        .join("object.json");
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
        &["task", "run", "--task-id", &task_id, "--graphql", &graphql],
    )?;
    assert!(
        stderr.contains("disabled"),
        "expected disabled-task error on stderr, got: {stderr}"
    );

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

fn lean_manual_dispatch_case() -> Result<&'static lean_vocab_test::LeanTriggerDispatchCase> {
    let cases = lean_vocab_test::lean_trigger_dispatch_cases();
    assert_eq!(
        lean_vocab_test::lean_trigger_dispatch_case_count(),
        cases.len(),
        "Lean trigger dispatch case-count sentinel drifted"
    );
    cases
        .iter()
        .find(|case| case.name == "manual_unconditional")
        .ok_or_else(|| anyhow!("Lean TriggerDispatch contracts did not emit manual_unconditional"))
}
