mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_running_runtime_without_restart() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");

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

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let behaviors_path = root.join("agent-behaviors.json");
    let mut behaviors = read_json_file(&behaviors_path)?;
    let updated_prompt = "Keep responses terse. Mention that desired state was applied.";
    behaviors[0]["system_prompt"] = Value::String(updated_prompt.to_string());
    write_json_file(&behaviors_path, &behaviors)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/remaining/agent_behaviors/update")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_apply =
        wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    system_prompt
                }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    let behavior_row = first_graphql_row(&response, "AgentBehavior")?;
    assert_eq!(
        behavior_row.get("system_prompt").and_then(Value::as_str),
        Some(updated_prompt)
    );

    let noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_noop = wait_for_runtime_quiescence(
        &graphql,
        &agent_did,
        generation_after_apply,
        Duration::from_secs(3),
    )
    .await?;
    assert_eq!(generation_after_noop, generation_after_apply);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_fresh_init_home_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-local-backend-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-apply-local-backend-{}", Uuid::new_v4().simple());

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
    assert!(exported
        .pointer("/inference_backends/0/last_probe")
        .is_none_or(Value::is_null));
    write_manifest_root_from_export(&root, &exported)?;

    let backends_path = root.join("inference-backends.json");
    let mut backends = read_json_file(&backends_path)?;
    let updated_endpoint = "http://127.0.0.1:9100/v1";
    backends[0]["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backends)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let explicit_home = home_dir.join(".defra-agent");
    let explicit_home_str = explicit_home
        .to_str()
        .ok_or_else(|| anyhow!("explicit home path is not UTF-8"))?;
    let applied = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexported = run_cli_json(
        &home_dir,
        &["config", "export", "--home", explicit_home_str],
    )?;
    assert_eq!(
        reexported
            .pointer("/inference_backends/0/endpoint")
            .and_then(Value::as_str),
        Some(updated_endpoint)
    );

    let noop = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_fresh_init_home_over_graphql() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!(
        "mock-apply-graphql-backend-model-{}",
        Uuid::new_v4().simple()
    );
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-graphql-backend-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");

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
    assert!(exported
        .pointer("/inference_backends/0/last_probe")
        .is_none_or(Value::is_null));
    write_manifest_root_from_export(&root, &exported)?;

    let backends_path = root.join("inference-backends.json");
    let mut backends = read_json_file(&backends_path)?;
    let updated_endpoint = "http://127.0.0.1:9200/v1";
    let backend_id = backends[0]
        .get("backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest backend is missing backend_id"))?
        .to_string();
    backends[0]["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backends)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(
                    filter: {{ backend_id: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    endpoint
                    probe_status
                    last_probe
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    let backend_row = first_graphql_row(&response, "InferenceBackend")?;
    assert_eq!(
        backend_row.get("endpoint").and_then(Value::as_str),
        Some(updated_endpoint)
    );
    assert_eq!(
        backend_row.get("probe_status").and_then(Value::as_str),
        Some("healthy")
    );
    assert!(backend_row.get("last_probe").is_none_or(Value::is_null));

    let noop = run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_tool_services_and_scheduled_tasks_end_to_end() -> Result<()> {
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

    let agent_did = exported
        .get("agent_did")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing agent_did"))?
        .to_string();
    let behavior_id = exported
        .pointer("/agent_principal/default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing default behavior id"))?
        .to_string();
    let service_id = format!("ops-mcp-{}", Uuid::new_v4().simple());
    let task_id = format!("nightly-audit-{}", Uuid::new_v4().simple());
    let service_path = root.join("tool-services").join("ops-mcp.json");
    let task_path = root.join("scheduled-tasks").join("nightly-audit.json");

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
            "agent_did": agent_did.clone(),
            "behavior_id": behavior_id.clone(),
            "name": "Nightly Audit",
            "prompt": "Audit the fleet state and summarize drift.",
            "interval_secs": 3600,
            "enabled": false
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
        validated
            .pointer("/counts/scheduled_tasks")
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
            .pointer("/counts/scheduled_tasks/create")
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
        applied
            .pointer("/applied/scheduled_tasks")
            .and_then(Value::as_u64),
        Some(1)
    );

    let task_response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ScheduledTask(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                    task_id
                    prompt
                    interval_secs
                    enabled
                    next_run_at
                    last_status
                    last_error
                    run_count
                }}
            }}"#,
            escape_graphql_string(&task_id),
        ),
    )
    .await?;
    let task_row = first_graphql_row(&task_response, "ScheduledTask")?;
    let initial_task_doc_id = task_row
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("scheduled task row missing _docID: {task_row}"))?
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
        noop.pointer("/applied/scheduled_tasks")
            .and_then(Value::as_u64),
        Some(0)
    );

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                update_ScheduledTask(
                    docID: "{doc_id}",
                    input: {{
                        next_run_at: "2026-04-15T00:00:00Z",
                        last_status: "error",
                        last_error: "boom",
                        run_count: 7
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = escape_graphql_string(&initial_task_doc_id),
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
                        version: "1.2.3",
                        updated_at: "2026-04-15T00:00:00Z"
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
        Some("noop")
    );
    assert_eq!(
        runtime_noop
            .pointer("/applied/tool_service_registries")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        runtime_noop
            .pointer("/applied/scheduled_tasks")
            .and_then(Value::as_u64),
        Some(0)
    );

    let mut task_manifest = read_json_file(&task_path)?;
    task_manifest["prompt"] =
        Value::String("Audit the fleet state for drift and incidents.".to_string());
    task_manifest["interval_secs"] = Value::from(7200);
    write_json_file(&task_path, &task_manifest)?;

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
        updated
            .pointer("/applied/scheduled_tasks")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexported = run_cli_json(&home_dir, &["config", "export"])?;
    assert_eq!(
        reexported
            .pointer("/tool_service_registries/0/hostname")
            .and_then(Value::as_str),
        Some("ops-router.internal")
    );
    assert_eq!(
        reexported
            .pointer("/scheduled_tasks/0/prompt")
            .and_then(Value::as_str),
        Some("Audit the fleet state for drift and incidents.")
    );
    assert_eq!(
        reexported
            .pointer("/scheduled_tasks/0/interval_secs")
            .and_then(Value::as_i64),
        Some(7200)
    );

    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{ delete_ScheduledTask(docID: "{}") {{ _docID }} }}"#,
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
        reapplied
            .pointer("/applied/scheduled_tasks")
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
            .pointer("/counts/scheduled_tasks/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}
