mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// End-to-end smoke test for the event-driven-tasks apply path.
///
/// Covers:
///   * creation of a `Task` + `Schedule` pair from a manifest,
///   * apply-owned field reconciliation after a manifest edit, and
///   * the critical invariant that apply NEVER writes runtime-owned
///     `Schedule` fields (`next_run_at`, `last_attempt_at`, `last_status`,
///     `last_error`, `fire_count`) — live scheduler state injected via
///     direct GraphQL mutation must survive a reapply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_tool_services_tasks_and_schedules_end_to_end() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-extra-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-extra-{}", Uuid::new_v4().simple());

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
    let service_id = format!("ops-mcp-{}", Uuid::new_v4().simple());
    let task_id = format!("nightly-audit-{}", Uuid::new_v4().simple());
    let schedule_id = format!("nightly-audit-schedule-{}", Uuid::new_v4().simple());
    let service_path = root
        .join("tool-services")
        .join(&service_id)
        .join("object.json");
    let task_path = root.join("tasks").join(&task_id).join("object.json");
    let schedule_path = root
        .join("schedules")
        .join(&schedule_id)
        .join("object.json");

    write_json_file(
        &service_path,
        &serde_json::json!({
            "service_id": service_id.clone(),
            "display_name": "Ops MCP",
            "description": "Operational tooling",
            "hostname": "ops.internal",
            "tailscale_ip": "100.64.0.10",
            "lan_ip": "192.168.1.10",
            "mcp_port": 8080,
            "mcp_path": "/mcp"
        }),
    )?;
    write_json_file(
        &task_path,
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Nightly Audit",
            "description": "Audit the fleet state and summarize drift.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Audit the fleet state and summarize drift.",
            "enabled": false,
        }),
    )?;
    write_json_file(
        &schedule_path,
        &serde_json::json!({
            "schedule_id": schedule_id.clone(),
            "task_id": task_id.clone(),
            "interval_secs": 3600,
            "enabled": false,
            "concurrency": "serial",
        }),
    )?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let validated = run_cli_json(&home_dir, &["config", "validate", "--root", root_str])?;
    assert_eq!(
        validated
            .pointer("/counts/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        validated.pointer("/counts/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        validated
            .pointer("/counts/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(
        &graphql,
        &format!("did:defra-agent:{agent_name}"),
        Duration::from_secs(30),
    )
    .await?;

    let planned = run_cli_json(&home_dir, &["config", "diff", "--root", root_str])?;
    assert_eq!(
        planned
            .pointer("/counts/tool_service_registries/create")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        planned
            .pointer("/counts/tasks/create")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        planned
            .pointer("/counts/schedules/create")
            .and_then(Value::as_u64),
        Some(1)
    );

    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/applied/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    // --- Task reconciled with apply-owned fields ---
    let task_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Task(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    task_id
                    name
                    description
                    behavior_id
                    prompt_template
                    enabled
                    output_schema_ref
                }}
            }}"#,
            escape_graphql_string(&task_id),
        ),
    )
    .await?;
    let task_row = first_graphql_row(&task_response, "Task")?;
    assert_eq!(
        task_row.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        task_row.get("name").and_then(Value::as_str),
        Some("Nightly Audit")
    );
    assert_eq!(
        task_row.get("behavior_id").and_then(Value::as_str),
        Some(behavior_id.as_str())
    );
    assert_eq!(
        task_row.get("prompt_template").and_then(Value::as_str),
        Some("Audit the fleet state and summarize drift.")
    );
    assert_eq!(
        task_row.get("enabled").and_then(Value::as_bool),
        Some(false)
    );
    let initial_task_doc_id = task_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Task row missing _docID: {task_row}"))?
        .to_string();

    // --- Schedule reconciled with apply-owned fields; runtime-owned fields
    //     start unset (the scheduler has not yet run) ---
    let schedule_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Schedule(filter: {{ schedule_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    schedule_id
                    task_id
                    interval_secs
                    enabled
                    concurrency
                    next_run_at
                    last_attempt_at
                    last_status
                    last_error
                    fire_count
                }}
            }}"#,
            escape_graphql_string(&schedule_id),
        ),
    )
    .await?;
    let schedule_row = first_graphql_row(&schedule_response, "Schedule")?;
    assert_eq!(
        schedule_row.get("schedule_id").and_then(Value::as_str),
        Some(schedule_id.as_str())
    );
    assert_eq!(
        schedule_row.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        schedule_row.get("interval_secs").and_then(Value::as_i64),
        Some(3600)
    );
    assert_eq!(
        schedule_row.get("enabled").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        schedule_row.get("concurrency").and_then(Value::as_str),
        Some("serial")
    );
    let initial_schedule_doc_id = schedule_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Schedule row missing _docID: {schedule_row}"))?
        .to_string();

    let service_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolServiceRegistry(filter: {{ service_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    service_id
                    description
                    hostname
                    status
                    version
                    updated_at
                }}
            }}"#,
            escape_graphql_string(&service_id),
        ),
    )
    .await?;
    let service_row = first_graphql_row(&service_response, "ToolServiceRegistry")?;
    let initial_service_doc_id = service_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tool service row missing _docID: {service_row}"))?
        .to_string();

    let noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(
        noop.pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        noop.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        noop.pointer("/applied/schedules").and_then(Value::as_u64),
        Some(0)
    );

    // Simulate the scheduler updating runtime-owned fields on the Schedule.
    // These must survive the next apply, since apply is forbidden from
    // touching runtime-owned fields.
    //
    // Note: DefraDB's GraphQL layer currently rejects manual string literals
    // for DateTime fields inserted via `update_*` (they're stored as Scalar
    // String and then fail the field-type check on the next update). We
    // exercise the apply-ownership boundary with the non-DateTime runtime
    // fields only (`last_status`, `last_error`, `fire_count`); the
    // next_run_at / last_attempt_at ownership is covered by the trigger_engine
    // e2e tests, which exercise the actual scheduler write path.
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_Schedule(
                    docID: "{doc_id}",
                    input: {{
                        last_status: "error",
                        last_error: "boom",
                        fire_count: 7
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = escape_graphql_string(&initial_schedule_doc_id),
        ),
    )
    .await?;
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_ToolServiceRegistry(
                    docID: "{doc_id}",
                    input: {{
                        status: "online",
                        version: "1.2.3"
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = escape_graphql_string(&initial_service_doc_id),
        ),
    )
    .await?;

    let runtime_noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        runtime_noop.get("status").and_then(Value::as_str),
        Some("noop"),
        "runtime-owned fields must not trigger apply-side drift"
    );
    assert_eq!(
        runtime_noop
            .pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        runtime_noop
            .pointer("/applied/tasks")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        runtime_noop
            .pointer("/applied/schedules")
            .and_then(Value::as_u64),
        Some(0)
    );

    // Confirm runtime-owned Schedule fields are preserved.
    let schedule_after_noop = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Schedule(filter: {{ schedule_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    next_run_at
                    last_attempt_at
                    last_status
                    last_error
                    fire_count
                }}
            }}"#,
            escape_graphql_string(&schedule_id),
        ),
    )
    .await?;
    let schedule_after_noop_row = first_graphql_row(&schedule_after_noop, "Schedule")?;
    assert_eq!(
        schedule_after_noop_row
            .get("_docID")
            .and_then(Value::as_str),
        Some(initial_schedule_doc_id.as_str()),
        "Schedule docID must be stable across noop apply"
    );
    assert_eq!(
        schedule_after_noop_row
            .get("last_status")
            .and_then(Value::as_str),
        Some("error"),
        "runtime-owned last_status must survive apply"
    );
    assert_eq!(
        schedule_after_noop_row
            .get("last_error")
            .and_then(Value::as_str),
        Some("boom"),
        "runtime-owned last_error must survive apply"
    );
    assert_eq!(
        schedule_after_noop_row
            .get("fire_count")
            .and_then(Value::as_i64),
        Some(7),
        "runtime-owned fire_count must survive apply"
    );

    // Edit the manifest; apply should reconcile apply-owned fields while
    // leaving runtime-owned Schedule fields untouched.
    let mut task_manifest = read_json_file(&task_path)?;
    task_manifest["prompt_template"] =
        Value::String("Audit the fleet state for drift and incidents.".to_string());
    task_manifest["description"] =
        Value::String("Audit the fleet state for drift and incidents.".to_string());
    write_json_file(&task_path, &task_manifest)?;

    let mut schedule_manifest = read_json_file(&schedule_path)?;
    schedule_manifest["interval_secs"] = Value::from(7200);
    schedule_manifest["concurrency"] = Value::String("latest_only".to_string());
    write_json_file(&schedule_path, &schedule_manifest)?;

    let mut service_manifest = read_json_file(&service_path)?;
    service_manifest["description"] =
        Value::String("Operational tooling and diagnostics".to_string());
    service_manifest["hostname"] = Value::String("ops-router.internal".to_string());
    write_json_file(&service_path, &service_manifest)?;

    let updated = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        updated.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        updated
            .pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        updated.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        updated
            .pointer("/applied/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    // Confirm apply-owned fields updated AND runtime-owned fields preserved.
    let schedule_after_update = graphql_query(
        &graphql,
        &format!(
            r#"{{
                Schedule(filter: {{ schedule_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    interval_secs
                    concurrency
                    last_status
                    last_error
                    fire_count
                }}
            }}"#,
            escape_graphql_string(&schedule_id),
        ),
    )
    .await?;
    let schedule_after_update_row = first_graphql_row(&schedule_after_update, "Schedule")?;
    assert_eq!(
        schedule_after_update_row
            .get("interval_secs")
            .and_then(Value::as_i64),
        Some(7200),
        "apply must update interval_secs"
    );
    assert_eq!(
        schedule_after_update_row
            .get("concurrency")
            .and_then(Value::as_str),
        Some("latest_only"),
        "apply must update concurrency"
    );
    assert_eq!(
        schedule_after_update_row
            .get("last_status")
            .and_then(Value::as_str),
        Some("error"),
        "runtime-owned last_status must survive apply-side update"
    );
    assert_eq!(
        schedule_after_update_row
            .get("fire_count")
            .and_then(Value::as_i64),
        Some(7),
        "runtime-owned fire_count must survive apply-side update"
    );

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ delete_Schedule(docID: "{}") {{ _docID }} }}"#,
            escape_graphql_string(&initial_schedule_doc_id),
        ),
    )
    .await?;
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ delete_Task(docID: "{}") {{ _docID }} }}"#,
            escape_graphql_string(&initial_task_doc_id),
        ),
    )
    .await?;
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ delete_ToolServiceRegistry(docID: "{}") {{ _docID }} }}"#,
            escape_graphql_string(&initial_service_doc_id),
        ),
    )
    .await?;

    let reapplied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        reapplied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(reapplied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        reapplied
            .pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        reapplied.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        reapplied
            .pointer("/applied/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    let exact = run_cli_json(&home_dir, &["config", "diff", "--root", root_str])?;
    assert_eq!(exact.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        exact
            .pointer("/counts/tool_service_registries/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        exact
            .pointer("/counts/tasks/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        exact
            .pointer("/counts/schedules/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}
