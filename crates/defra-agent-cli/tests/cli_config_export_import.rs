mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_import_round_trips_offline_and_requires_override() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home = tempdir.path().join("source-home");
    let target_home = tempdir.path().join("target-home");
    fs::create_dir_all(&source_home)?;
    fs::create_dir_all(&target_home)?;

    let model_name = format!("mock-export-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-export-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let export_path = tempdir.path().join("agent-config.json");

    run_init_json(
        &source_home,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let exported = run_cli_json(&source_home, &["config", "export"])?;
    assert_eq!(
        exported.get("format").and_then(Value::as_str),
        Some("defra-agent-config/v2")
    );
    assert_eq!(
        exported.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        exported
            .pointer("/agent_behaviors")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        exported
            .pointer("/tool_selections")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        exported
            .pointer("/inference_backends")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    fs::write(&export_path, serde_json::to_vec_pretty(&exported)?)
        .context("writing config export fixture")?;

    let imported = run_cli_json(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
        ],
    )?;
    assert_eq!(
        imported.get("status").and_then(Value::as_str),
        Some("imported")
    );
    assert_eq!(
        imported
            .pointer("/counts/agent_principal")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        imported
            .pointer("/counts/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexported = run_cli_json(
        &target_home,
        &["config", "export", "--agent-did", &agent_did],
    )?;
    assert_eq!(
        reexported.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        reexported
            .pointer("/agent_principal/default_behavior_id")
            .and_then(Value::as_str),
        exported
            .pointer("/agent_principal/default_behavior_id")
            .and_then(Value::as_str)
    );
    assert_eq!(
        reexported
            .pointer("/agent_behaviors/0/behavior_id")
            .and_then(Value::as_str),
        exported
            .pointer("/agent_behaviors/0/behavior_id")
            .and_then(Value::as_str)
    );

    let stderr = run_cli_failure_stderr(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
        ],
    )?;
    assert!(
        stderr.contains("defra-agent config import --override"),
        "expected override guidance in stderr, got:\n{stderr}"
    );

    let overridden = run_cli_json(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
            "--override",
        ],
    )?;
    assert_eq!(
        overridden.get("override").and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_import_round_trips_tool_services_and_tasks() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home = tempdir.path().join("source-home");
    let target_home = tempdir.path().join("target-home");
    fs::create_dir_all(&source_home)?;
    fs::create_dir_all(&target_home)?;

    let model_name = format!("mock-export-extra-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-export-extra-{}", Uuid::new_v4().simple());
    let export_path = tempdir.path().join("agent-config-extra.json");

    run_init_json(
        &source_home,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let mut seeded_bundle = run_cli_json(&source_home, &["config", "export"])?;
    let behavior_id = seeded_bundle
        .pointer("/agent_principal/default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("seeded export missing default behavior id"))?
        .to_string();
    let service_id = format!("ops-mcp-{}", Uuid::new_v4().simple());
    let task_id = format!("nightly-audit-{}", Uuid::new_v4().simple());
    let schedule_id = format!("{task_id}-hourly");

    seeded_bundle
        .get_mut("tool_service_registries")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("seeded export missing tool_service_registries array"))?
        .push(serde_json::json!({
            "service_id": service_id.clone(),
            "display_name": "Ops MCP",
            "description": "Operational tooling",
            "hostname": "ops.internal",
            "tailscale_ip": "100.64.0.10",
            "lan_ip": "192.168.1.10",
            "mcp_port": 8080,
            "mcp_path": "/mcp"
        }));
    seeded_bundle
        .get_mut("tasks")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("seeded export missing tasks array"))?
        .push(serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Nightly Audit",
            "description": null,
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Audit the fleet state and summarize drift.",
            "enabled": false,
            "output_schema_ref": null
        }));
    seeded_bundle
        .get_mut("schedules")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("seeded export missing schedules array"))?
        .push(serde_json::json!({
            "schedule_id": schedule_id.clone(),
            "task_id": task_id.clone(),
            "interval_secs": 3600,
            "enabled": false,
            "concurrency": "serial"
        }));

    fs::write(&export_path, serde_json::to_vec_pretty(&seeded_bundle)?)
        .context("writing config export fixture with task and tool service")?;

    let seeded_import = run_cli_json(
        &source_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
            "--override",
        ],
    )?;
    assert_eq!(
        seeded_import
            .pointer("/counts/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        seeded_import
            .pointer("/counts/tasks")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        seeded_import
            .pointer("/counts/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    let exported = run_cli_json(&source_home, &["config", "export"])?;
    assert_eq!(
        exported
            .pointer("/tool_service_registries/0/service_id")
            .and_then(Value::as_str),
        Some(service_id.as_str())
    );
    assert_eq!(
        exported.pointer("/tasks/0/task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        exported
            .pointer("/schedules/0/schedule_id")
            .and_then(Value::as_str),
        Some(schedule_id.as_str())
    );
    assert!(
        exported
            .pointer("/tool_service_registries/0/status")
            .is_none(),
        "tool-service export should omit runtime status: {exported}"
    );
    assert!(
        exported
            .pointer("/tool_service_registries/0/tools")
            .is_none(),
        "tool-service export should omit discovered tools: {exported}"
    );

    fs::write(&export_path, serde_json::to_vec_pretty(&exported)?)
        .context("writing round-trip config export fixture")?;

    let imported = run_cli_json(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
        ],
    )?;
    assert_eq!(
        imported
            .pointer("/counts/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        imported.pointer("/counts/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        imported
            .pointer("/counts/schedules")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexported = run_cli_json(
        &target_home,
        &[
            "config",
            "export",
            "--agent-did",
            seeded_bundle
                .get("agent_did")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("seeded bundle missing agent_did"))?,
        ],
    )?;
    assert_eq!(
        reexported
            .pointer("/tool_service_registries/0/service_id")
            .and_then(Value::as_str),
        Some(service_id.as_str())
    );
    assert_eq!(
        reexported
            .pointer("/tasks/0/task_id")
            .and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        reexported
            .pointer("/tasks/0/prompt_template")
            .and_then(Value::as_str),
        Some("Audit the fleet state and summarize drift.")
    );
    assert_eq!(
        reexported
            .pointer("/schedules/0/schedule_id")
            .and_then(Value::as_str),
        Some(schedule_id.as_str())
    );

    Ok(())
}
