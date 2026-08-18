use crate::support::*;

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

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
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
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_accepts_explicit_empty_tool_selection_lists_twice() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-empty-list-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-empty-list-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();
    let behavior = read_json_file(
        &root
            .join("agent-behaviors")
            .join(&behavior_id)
            .join("object.json"),
    )?;
    let selection_id = behavior
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing tool_selection_id after export"))?
        .to_string();
    let selection_path = root
        .join("tool-selections")
        .join(&selection_id)
        .join("object.json");
    let mut selection = read_json_file(&selection_path)?;
    selection["display_name"] = Value::String("Empty list regression".to_string());
    selection["read_only_command_allowlist"] = Value::Array(vec![
        Value::String("jq".into()),
        Value::String("echo".into()),
    ]);
    for field in [
        "command_allowed_argv_prefixes",
        "command_forbidden_argv_prefixes",
        "cli_tool_names",
        "allowed_mcp_service_ids",
        "delegate_to",
        "backgroundable_tool_names",
        "subagent_targets",
        "defra_query_collections",
    ] {
        selection[field] = Value::Array(Vec::new());
    }
    selection["subagent_spawn_enabled"] = Value::Bool(false);
    selection["subagent_steering_enabled"] = Value::Bool(false);
    selection["subagent_background_enabled"] = Value::Bool(false);
    selection["subagent_allow_cross_deployment"] = Value::Bool(false);
    selection["cross_deployment_spawn_timeout_seconds"] = Value::Null;
    write_json_file(&selection_path, &selection)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let validated = run_cli_json(&home_dir, &["config", "validate", "--root", root_str])?;
    assert_eq!(validated.get("ok").and_then(Value::as_bool), Some(true));

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        applied
            .pointer("/applied/tool_selections")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reapplied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        reapplied.get("status").and_then(Value::as_str),
        Some("noop")
    );
    assert_eq!(
        reapplied
            .pointer("/applied/tool_selections")
            .and_then(Value::as_u64),
        Some(0)
    );

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    selection_id
                    display_name
                    command_allowed_argv_prefixes
                    command_forbidden_argv_prefixes
                    read_only_command_allowlist
                    cli_tool_names
                    allowed_mcp_service_ids
                    delegate_to
                    backgroundable_tool_names
                    subagent_targets
                    subagent_spawn_enabled
                    subagent_steering_enabled
                    subagent_background_enabled
                    subagent_allow_cross_deployment
                    cross_deployment_spawn_timeout_seconds
                    defra_query_collections
                }}
            }}"#,
            escape_graphql_string(&selection_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "ToolSelection")?;
    assert_eq!(
        row.get("display_name").and_then(Value::as_str),
        Some("Empty list regression")
    );
    for field in [
        "command_allowed_argv_prefixes",
        "command_forbidden_argv_prefixes",
        "cli_tool_names",
        "allowed_mcp_service_ids",
        "delegate_to",
        "backgroundable_tool_names",
        "subagent_targets",
        "defra_query_collections",
    ] {
        assert!(
            row.get(field).is_none_or(|value| {
                value.is_null() || value.as_array().is_some_and(Vec::is_empty)
            }),
            "expected {field} to query back as null or empty array, got {row}"
        );
    }
    assert_eq!(
        row.get("read_only_command_allowlist")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>()),
        Some(vec!["echo", "jq"]),
        "expected read_only_command_allowlist to round-trip (sorted by normalize): {row}"
    );
    assert_eq!(
        row.get("subagent_allow_cross_deployment")
            .and_then(Value::as_bool),
        Some(false),
        "expected subagent_allow_cross_deployment to stay disabled: {row}"
    );
    assert!(
        row.get("cross_deployment_spawn_timeout_seconds")
            .is_none_or(Value::is_null),
        "expected cross_deployment_spawn_timeout_seconds to stay null: {row}"
    );

    Ok(())
}

/// End-to-end test for the EventTrigger apply path.
///
/// Covers the runtime-ownership contract for `EventTrigger`:
///   * creation of a `Task` + `EventTrigger` pair from a manifest,
///   * apply-owned field reconciliation after a manifest edit, and
///   * the critical invariant that apply NEVER writes runtime-owned
///     `EventTrigger` fields (`last_attempt_at`,
///     `last_fired_source_doc_id`, `last_status`, `last_error`,
///     `fire_count`) — live trigger-engine state injected via direct
///     GraphQL mutation must survive a reapply, even when apply-owned
///     fields change in the same apply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_event_triggers_end_to_end() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-trigger-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-trigger-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();
    let task_id = format!("greet-signup-{}", Uuid::new_v4().simple());
    let trigger_id = format!("on-signup-created-{}", Uuid::new_v4().simple());
    let task_path = root.join("tasks").join(&task_id).join("object.json");
    let trigger_path = root
        .join("event_triggers")
        .join(&trigger_id)
        .join("object.json");

    write_json_file(
        &task_path,
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Greet New Signup",
            "description": "Send a personalized welcome to a new backend signup.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Greet new backend {{ doc.name }}.",
            "enabled": true,
        }),
    )?;
    write_json_file(
        &trigger_path,
        &serde_json::json!({
            "trigger_id": trigger_id.clone(),
            "task_id": task_id.clone(),
            "source_collection": "InferenceBackend",
            "event_kind": "created",
            "filter": "{ enabled: { _eq: true } }",
            "enabled": true,
            "concurrency": "serial",
        }),
    )?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let validated = run_cli_json(&home_dir, &["config", "validate", "--root", root_str])?;
    assert_eq!(
        validated.pointer("/counts/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        validated
            .pointer("/counts/event_triggers")
            .and_then(Value::as_u64),
        Some(1)
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let planned = run_cli_json(&home_dir, &["config", "diff", "--root", root_str])?;
    assert_eq!(
        planned
            .pointer("/counts/tasks/create")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        planned
            .pointer("/counts/event_triggers/create")
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
        applied.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/applied/event_triggers")
            .and_then(Value::as_u64),
        Some(1)
    );

    let trigger_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    trigger_id
                    task_id
                    source_collection
                    event_kind
                    filter
                    enabled
                    concurrency
                    last_attempt_at
                    last_fired_source_doc_id
                    last_status
                    last_error
                    fire_count
                }}
            }}"#,
            escape_graphql_string(&trigger_id),
        ),
    )
    .await?;
    let trigger_row = first_graphql_row(&trigger_response, "EventTrigger")?;
    assert_eq!(
        trigger_row.get("trigger_id").and_then(Value::as_str),
        Some(trigger_id.as_str())
    );
    assert_eq!(
        trigger_row.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        trigger_row.get("source_collection").and_then(Value::as_str),
        Some("InferenceBackend")
    );
    assert_eq!(
        trigger_row.get("event_kind").and_then(Value::as_str),
        Some("created")
    );
    assert_eq!(
        trigger_row.get("filter").and_then(Value::as_str),
        Some("{ enabled: { _eq: true } }")
    );
    assert_eq!(
        trigger_row.get("enabled").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        trigger_row.get("concurrency").and_then(Value::as_str),
        Some("serial")
    );
    assert!(
        trigger_row
            .get("last_status")
            .map(Value::is_null)
            .unwrap_or(true),
        "last_status should be null before any fire: {trigger_row}"
    );
    assert!(
        trigger_row
            .get("fire_count")
            .map(Value::is_null)
            .unwrap_or(true),
        "fire_count should be null before any fire: {trigger_row}"
    );
    assert!(
        trigger_row
            .get("last_fired_source_doc_id")
            .map(Value::is_null)
            .unwrap_or(true),
        "last_fired_source_doc_id should be null before any fire: {trigger_row}"
    );
    let initial_trigger_doc_id = trigger_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("EventTrigger row missing _docID: {trigger_row}"))?
        .to_string();

    let noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(
        noop.pointer("/applied/event_triggers")
            .and_then(Value::as_u64),
        Some(0)
    );

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_EventTrigger(
                    docID: "{doc_id}",
                    input: {{
                        last_status: "fired",
                        last_error: null,
                        fire_count: 3,
                        last_fired_source_doc_id: "src-abc"
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = escape_graphql_string(&initial_trigger_doc_id),
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
            .pointer("/applied/event_triggers")
            .and_then(Value::as_u64),
        Some(0)
    );

    let trigger_after_noop = graphql_query(
        &graphql,
        &format!(
            r#"{{
                EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    last_status
                    last_error
                    fire_count
                    last_fired_source_doc_id
                }}
            }}"#,
            escape_graphql_string(&trigger_id),
        ),
    )
    .await?;
    let trigger_after_noop_row = first_graphql_row(&trigger_after_noop, "EventTrigger")?;
    assert_eq!(
        trigger_after_noop_row.get("_docID").and_then(Value::as_str),
        Some(initial_trigger_doc_id.as_str()),
        "EventTrigger docID must be stable across noop apply"
    );
    assert_eq!(
        trigger_after_noop_row
            .get("last_status")
            .and_then(Value::as_str),
        Some("fired"),
        "runtime-owned last_status must survive noop apply"
    );
    assert_eq!(
        trigger_after_noop_row
            .get("fire_count")
            .and_then(Value::as_i64),
        Some(3),
        "runtime-owned fire_count must survive noop apply"
    );
    assert_eq!(
        trigger_after_noop_row
            .get("last_fired_source_doc_id")
            .and_then(Value::as_str),
        Some("src-abc"),
        "runtime-owned last_fired_source_doc_id must survive noop apply"
    );

    let mut trigger_manifest = read_json_file(&trigger_path)?;
    trigger_manifest["filter"] =
        Value::String("{ enabled: { _eq: true }, provider_kind: { _eq: \"openai\" } }".to_string());
    trigger_manifest["enabled"] = Value::from(false);
    trigger_manifest["concurrency"] = Value::String("latest_only".to_string());
    write_json_file(&trigger_path, &trigger_manifest)?;

    let updated = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        updated.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        updated
            .pointer("/applied/event_triggers")
            .and_then(Value::as_u64),
        Some(1)
    );

    let trigger_after_update = graphql_query(
        &graphql,
        &format!(
            r#"{{
                EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    filter
                    enabled
                    concurrency
                    last_status
                    last_error
                    fire_count
                    last_fired_source_doc_id
                }}
            }}"#,
            escape_graphql_string(&trigger_id),
        ),
    )
    .await?;
    let trigger_after_update_row = first_graphql_row(&trigger_after_update, "EventTrigger")?;
    assert_eq!(
        trigger_after_update_row
            .get("filter")
            .and_then(Value::as_str),
        Some("{ enabled: { _eq: true }, provider_kind: { _eq: \"openai\" } }"),
        "apply must update filter"
    );
    assert_eq!(
        trigger_after_update_row
            .get("enabled")
            .and_then(Value::as_bool),
        Some(false),
        "apply must update enabled"
    );
    assert_eq!(
        trigger_after_update_row
            .get("concurrency")
            .and_then(Value::as_str),
        Some("latest_only"),
        "apply must update concurrency"
    );
    assert_eq!(
        trigger_after_update_row
            .get("last_status")
            .and_then(Value::as_str),
        Some("fired"),
        "runtime-owned last_status must survive apply-side update"
    );
    assert_eq!(
        trigger_after_update_row
            .get("fire_count")
            .and_then(Value::as_i64),
        Some(3),
        "runtime-owned fire_count must survive apply-side update"
    );
    assert_eq!(
        trigger_after_update_row
            .get("last_fired_source_doc_id")
            .and_then(Value::as_str),
        Some("src-abc"),
        "runtime-owned last_fired_source_doc_id must survive apply-side update"
    );

    Ok(())
}

struct LiveValidationFixture {
    home_dir: std::path::PathBuf,
    root: std::path::PathBuf,
    trigger_id: String,
    task_path: std::path::PathBuf,
    trigger_path: std::path::PathBuf,
    _serve: ServeProcess,
    _mock_endpoint: MockModelEndpoint,
    _tempdir: tempfile::TempDir,
}

async fn prepare_live_validation_fixture(suffix: &str) -> Result<LiveValidationFixture> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-live-validate-model-{suffix}");
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-live-validate-{suffix}");

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

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();
    let task_id = format!("greet-{suffix}");
    let trigger_id = format!("on-created-{suffix}");
    let task_path = root.join("tasks").join(&task_id).join("object.json");
    let trigger_path = root
        .join("event_triggers")
        .join(&trigger_id)
        .join("object.json");

    write_json_file(
        &task_path,
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Greet",
            "description": "Greet a new signup.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Greet new backend {{ doc.name }}.",
            "enabled": true,
        }),
    )?;
    write_json_file(
        &trigger_path,
        &serde_json::json!({
            "trigger_id": trigger_id.clone(),
            "task_id": task_id.clone(),
            "source_collection": "InferenceBackend",
            "event_kind": "created",
            "filter": "{ enabled: { _eq: true } }",
            "enabled": true,
            "concurrency": "serial",
        }),
    )?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    Ok(LiveValidationFixture {
        home_dir,
        root,
        trigger_id,
        task_path,
        trigger_path,
        _serve: serve,
        _mock_endpoint: mock_endpoint,
        _tempdir: tempdir,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_rejects_event_trigger_with_malformed_filter() -> Result<()> {
    let fx = prepare_live_validation_fixture(&Uuid::new_v4().simple().to_string()).await?;

    let mut trigger_manifest = read_json_file(&fx.trigger_path)?;
    trigger_manifest["filter"] = Value::String("{ not_a_field: }".to_string());
    write_json_file(&fx.trigger_path, &trigger_manifest)?;

    let root_str = fx
        .root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let stderr = run_cli_failure_stderr(&fx.home_dir, &["config", "apply", "--root", root_str])?;
    assert!(
        stderr.contains(&format!(
            "event_trigger {} filter syntax error",
            fx.trigger_id
        )),
        "expected filter syntax error for trigger {} in stderr, got: {stderr}",
        fx.trigger_id,
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_rejects_event_trigger_template_referencing_unknown_doc_field() -> Result<()> {
    let fx = prepare_live_validation_fixture(&Uuid::new_v4().simple().to_string()).await?;

    let mut task_manifest = read_json_file(&fx.task_path)?;
    task_manifest["prompt_template"] = Value::String("Greet {{ doc.nonexistent }}.".to_string());
    write_json_file(&fx.task_path, &task_manifest)?;

    let root_str = fx
        .root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let stderr = run_cli_failure_stderr(&fx.home_dir, &["config", "apply", "--root", root_str])?;
    assert!(
        stderr.contains(&format!(
            "event_trigger {} template references doc.nonexistent but InferenceBackend has no such field",
            fx.trigger_id
        )),
        "expected doc.nonexistent rejection for trigger {} in stderr, got: {stderr}",
        fx.trigger_id,
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_accepts_event_trigger_with_valid_filter_and_doc_paths() -> Result<()> {
    let fx = prepare_live_validation_fixture(&Uuid::new_v4().simple().to_string()).await?;

    let root_str = fx
        .root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(&fx.home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/event_triggers")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied.pointer("/applied/tasks").and_then(Value::as_u64),
        Some(1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_round_trips_write_tools_without_drift() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-write-tools-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-write-tools-{}", Uuid::new_v4().simple());

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

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();
    let behavior = read_json_file(
        &root
            .join("agent-behaviors")
            .join(&behavior_id)
            .join("object.json"),
    )?;
    let selection_id = behavior
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing tool_selection_id after export"))?
        .to_string();
    let selection_path = root
        .join("tool-selections")
        .join(&selection_id)
        .join("object.json");
    let mut selection = read_json_file(&selection_path)?;
    selection["display_name"] = Value::String("write_tools round-trip".to_string());
    selection["write_tools"] = serde_json::json!([
        {
            "tool_name": "request_action",
            "collection": "ActionRequest",
            "description": "Request a bounded action",
            "fields": [
                { "name": "title", "required": true },
                { "name": "detail", "required": false }
            ]
        }
    ]);
    write_json_file(&selection_path, &selection)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let validated = run_cli_json(&home_dir, &["config", "validate", "--root", root_str])?;
    assert_eq!(
        validated.get("ok").and_then(Value::as_bool),
        Some(true),
        "manifest with write_tools must validate: {validated}"
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 1, Duration::from_secs(2)).await?;

    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied"),
        "first apply must write the selection: {applied}"
    );
    assert_eq!(
        applied
            .pointer("/applied/tool_selections")
            .and_then(Value::as_u64),
        Some(1),
        "first apply must touch exactly the one tool selection: {applied}"
    );

    let reapplied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        reapplied.get("status").and_then(Value::as_str),
        Some("noop"),
        "re-apply must be a noop (write_tools persisted and matches): {reapplied}"
    );
    assert_eq!(
        reapplied
            .pointer("/applied/tool_selections")
            .and_then(Value::as_u64),
        Some(0),
        "re-apply must not re-write the selection: {reapplied}"
    );

    let exact = run_cli_json(&home_dir, &["config", "diff", "--root", root_str])?;
    assert_eq!(
        exact.get("ok").and_then(Value::as_bool),
        Some(true),
        "diff must be clean after write_tools apply: {exact}"
    );
    assert_eq!(
        exact
            .pointer("/counts/tool_selections/unchanged")
            .and_then(Value::as_u64),
        Some(1),
        "the write_tools selection must count as unchanged: {exact}"
    );
    assert_eq!(
        exact
            .pointer("/counts/tool_selections/update")
            .and_then(Value::as_u64),
        Some(0),
        "no spurious update for the write_tools selection: {exact}"
    );

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    selection_id
                    write_tools
                }}
            }}"#,
            escape_graphql_string(&selection_id),
        ),
    )
    .await?;
    let row = first_graphql_row(&response, "ToolSelection")?;
    let stored = row
        .get("write_tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("write_tools did not query back as a list: {row}"))?;
    assert_eq!(
        stored.len(),
        1,
        "expected one stored write_tools entry: {row}"
    );
    let decl: Value = serde_json::from_str(
        stored[0]
            .as_str()
            .ok_or_else(|| anyhow!("stored write_tools entry is not a JSON string: {row}"))?,
    )
    .context("stored write_tools entry must be JSON")?;
    assert_eq!(
        decl.get("tool_name").and_then(Value::as_str),
        Some("request_action"),
        "stored decl tool_name must round-trip: {decl}"
    );
    assert_eq!(
        decl.get("collection").and_then(Value::as_str),
        Some("ActionRequest"),
        "stored decl collection must round-trip: {decl}"
    );
    assert_eq!(
        decl.pointer("/fields/0/name").and_then(Value::as_str),
        Some("title"),
        "stored decl field must round-trip: {decl}"
    );

    Ok(())
}
