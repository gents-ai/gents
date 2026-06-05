mod support;
use support::*;

use std::fs;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_accepts_normalized_manifest_root() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    let default_behavior_id = "default".to_string();
    let tool_selection_id = format!("{default_behavior_id}-tools");

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Default Agent",
            "default_behavior_id": default_behavior_id.clone(),
            "enabled": true
        }),
    )?;
    {
        let dir = root
            .join("agent-behaviors")
            .join(default_behavior_id.as_str());
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "behavior_id": default_behavior_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Default",
                "system_prompt": "Keep responses short.",
                "backend_id": "default-backend",
                "model_name": "mock-model",
                "tool_selection_id": tool_selection_id.clone(),
                "inference_profile_id": null,
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }),
        )?;
    }
    {
        let dir = root
            .join("tool-selections")
            .join(tool_selection_id.as_str());
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "selection_id": tool_selection_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Standard",
                "enable_file_tools": true,
                "file_tools_mode": "ReadOnly",
                "enable_bash": true,
                "bash_mode": "ReadOnly",
                "command_execution_policy": "read_only",
                "command_network_mode": "disabled",
                "command_allowed_argv_prefixes": ["[\"git\",\"status\"]"],
                "command_forbidden_argv_prefixes": ["git commit"],
                "cli_tool_names": [],
                "enable_meta_tools": true,
                "allowed_mcp_service_ids": ["x-data"]
            }),
        )?;
    }
    {
        let dir = root.join("inference-backends").join("default-backend");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "backend_id": "default-backend",
                "name": "default-backend",
                "endpoint": "http://127.0.0.1:8000/v1",
                "api_key_env_var": "DEFRA_AGENT_TEST_MANIFEST_API_KEY",
                "max_concurrent": 2,
                "max_queue_depth": 100,
                "enabled": true,
                "models": ["mock-model"]
            }),
        )?;
    }

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("validated")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output
            .pointer("/counts/agent_principal")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/tool_selections")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_profiles")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        output.get("errors").and_then(Value::as_array).map(Vec::len),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_reports_reference_errors_and_fails_nonzero() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("broken");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Broken Agent",
            "default_behavior_id": "default".to_string(),
            "enabled": true
        }),
    )?;
    {
        let dir = root.join("agent-behaviors").join("other-behavior");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "behavior_id": "other-behavior",
                "agent_did": agent_did.clone(),
                "display_name": "Other",
                "system_prompt": "Broken config.",
                "backend_id": "missing-backend",
                "model_name": "mock-model",
                "tool_selection_id": "missing-tools",
                "inference_profile_id": "missing-profile",
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }),
        )?;
    }
    // Empty collections: no subdirectories needed (missing dir = zero docs)

    let output = run_cli_failure_stdout_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("invalid")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(false));
    let errors = output
        .get("errors")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("validate output missing errors array: {output}"))?;
    let messages = errors
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        messages.contains("default_behavior_id"),
        "expected default behavior validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing backend_id missing-backend"),
        "expected missing backend validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing tool_selection_id missing-tools"),
        "expected missing tool selection validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing inference_profile_id missing-profile"),
        "expected missing profile validation error, got:\n{messages}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_reports_command_policy_errors() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("policy-errors");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Policy Agent",
            "default_behavior_id": "missing-behavior",
            "enabled": true
        }),
    )?;

    let dir = root.join("tool-selections").join("policy-tools");
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "selection_id": "policy-tools",
            "agent_did": agent_did,
            "display_name": "Policy Tools",
            "enable_file_tools": true,
            "file_tools_mode": "ReadOnly",
            "enable_bash": true,
            "bash_mode": "ReadOnly",
            "command_execution_policy": "side_effects",
            "command_network_mode": "maybe",
            "command_allowed_argv_prefixes": ["[\"git\", \"\"]"],
            "command_forbidden_argv_prefixes": ["["],
            "cli_tool_names": [],
            "enable_meta_tools": true
        }),
    )?;

    let output = run_cli_failure_stdout_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    let errors = output
        .get("errors")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("validate output missing errors array: {output}"))?;
    let messages = errors
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        messages.contains("invalid command_execution_policy"),
        "expected invalid command_execution_policy rejection, got:\n{messages}"
    );
    assert!(
        messages.contains("invalid command_network_mode"),
        "expected invalid command_network_mode rejection, got:\n{messages}"
    );
    assert!(
        messages.contains(
            "command_allowed_argv_prefixes JSON entry must contain non-empty argv tokens"
        ),
        "expected invalid allowed-prefix JSON rejection, got:\n{messages}"
    );
    assert!(
        messages.contains("command_forbidden_argv_prefixes JSON entry is invalid"),
        "expected invalid forbidden-prefix JSON rejection, got:\n{messages}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_accepts_tool_services_dir_and_tasks_dir() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("fleet");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    let default_behavior_id = "default".to_string();
    let tool_selection_id = format!("{default_behavior_id}-tools");

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Fleet Agent",
            "default_behavior_id": default_behavior_id.clone(),
            "enabled": true
        }),
    )?;
    {
        let dir = root
            .join("agent-behaviors")
            .join(default_behavior_id.as_str());
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "behavior_id": default_behavior_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Default",
                "system_prompt": "Stay focused.",
                "backend_id": "default-backend",
                "model_name": "mock-model",
                "tool_selection_id": tool_selection_id.clone(),
                "inference_profile_id": null,
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }),
        )?;
    }
    {
        let dir = root
            .join("tool-selections")
            .join(tool_selection_id.as_str());
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "selection_id": tool_selection_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Standard",
                "enable_file_tools": true,
                "file_tools_mode": "ReadOnly",
                "enable_bash": true,
                "bash_mode": "ReadOnly",
                "cli_tool_names": [],
                "enable_meta_tools": true
            }),
        )?;
    }
    {
        let dir = root.join("inference-backends").join("default-backend");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "backend_id": "default-backend",
                "name": "default-backend",
                "endpoint": "http://127.0.0.1:8000/v1",
                "api_key_env_var": "DEFRA_AGENT_TEST_MANIFEST_API_KEY",
                "max_concurrent": 2,
                "max_queue_depth": 100,
                "enabled": true,
                "models": ["mock-model"]
            }),
        )?;
    }
    {
        let dir = root.join("tool-services").join("ops-mcp");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "service_id": "ops-mcp",
                "display_name": "Ops MCP",
                "description": "Operational tooling",
                "hostname": "ops.internal",
                "tailscale_ip": "100.64.0.10",
                "lan_ip": "192.168.1.10",
                "mcp_port": 8080,
                "mcp_path": "/mcp"
            }),
        )?;
    }
    {
        let dir = root.join("tasks").join("nightly-audit");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "task_id": "nightly-audit",
                "name": "Nightly Audit",
                "description": null,
                "behavior_id": default_behavior_id.clone(),
                "prompt_template": "Audit the fleet state and summarize drift.",
                "enabled": false,
                "output_schema_ref": null
            }),
        )?;
    }
    {
        let dir = root.join("schedules").join("nightly-audit-hourly");
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "schedule_id": "nightly-audit-hourly",
                "task_id": "nightly-audit",
                "interval_secs": 3600,
                "enabled": false,
                "concurrency": "serial"
            }),
        )?;
    }

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("validated")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output
            .pointer("/counts/tool_service_registries")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output.pointer("/counts/tasks").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output.pointer("/counts/schedules").and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_without_binding_keeps_manifest_agent_did_authoritative() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1-steward");
    fs::create_dir_all(&home_dir)?;

    let placeholder_did = "did:defra-agent:mini-1-steward";
    write_rebindable_manifest_root(&root, placeholder_did)?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("validated")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(placeholder_did)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_bind_home_accepts_placeholder_agent_did() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1-steward");
    fs::create_dir_all(&home_dir)?;

    let init = run_init_json(
        &home_dir,
        &["--identity-only", "--agent-name", "mini-1-steward"],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let explicit_home = home_dir.join(".defra-agent");
    write_json_file(
        &explicit_home.join("runtime.json"),
        &serde_json::json!({
            "home": explicit_home.to_string_lossy(),
            "graphql": "http://127.0.0.1:9191/api/v0/graphql",
            "agent_name": "mini-1-steward",
            "agent_did": "did:defra-agent:mini-1-steward",
            "default_behavior_id": "default"
        }),
    )?;
    write_rebindable_manifest_root(&root, "did:defra-agent:mini-1-steward")?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
            "--home",
            explicit_home.to_str().expect("utf-8 home"),
            "--bind-agent-did",
            "home",
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("validated")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_bind_home_rejects_concrete_manifest_did_without_force() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1-steward");
    fs::create_dir_all(&home_dir)?;

    run_init_json(
        &home_dir,
        &["--identity-only", "--agent-name", "mini-1-steward"],
    )?;
    let explicit_home = home_dir.join(".defra-agent");
    write_rebindable_manifest_root(&root, &format!("did:key:z{}", Uuid::new_v4().simple()))?;

    let stderr = run_cli_failure_stderr(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
            "--home",
            explicit_home.to_str().expect("utf-8 home"),
            "--bind-agent-did",
            "home",
        ],
    )?;
    assert!(
        stderr.contains("concrete agent DID"),
        "expected concrete manifest DID mismatch, got:\n{stderr}"
    );
    assert!(
        stderr.contains("--force-rebind-concrete-did"),
        "expected scoped force hint, got:\n{stderr}"
    );

    Ok(())
}

fn write_rebindable_manifest_root(root: &std::path::Path, agent_did: &str) -> Result<()> {
    fs::create_dir_all(root)?;
    let default_behavior_id = "default";
    let tool_selection_id = "default-tools";

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did,
            "display_name": "Mini 1 Steward",
            "default_behavior_id": default_behavior_id,
            "enabled": true
        }),
    )?;
    write_json_file(
        &root
            .join("agent-behaviors")
            .join(default_behavior_id)
            .join("object.json"),
        &serde_json::json!({
            "behavior_id": default_behavior_id,
            "agent_did": agent_did,
            "display_name": "Default",
            "system_prompt": "Keep responses short.",
            "backend_id": "default-backend",
            "model_name": "mock-model",
            "tool_selection_id": tool_selection_id,
            "inference_profile_id": null,
            "compaction_strategy": null,
            "compaction_threshold": null,
            "enabled": true
        }),
    )?;
    write_json_file(
        &root
            .join("tool-selections")
            .join(tool_selection_id)
            .join("object.json"),
        &serde_json::json!({
            "selection_id": tool_selection_id,
            "agent_did": agent_did,
            "display_name": "Standard",
            "enable_file_tools": true,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": true,
            "bash_mode": "ReadOnly",
            "cli_tool_names": [],
            "enable_meta_tools": true
        }),
    )?;
    write_json_file(
        &root
            .join("inference-backends")
            .join("default-backend")
            .join("object.json"),
        &serde_json::json!({
            "backend_id": "default-backend",
            "name": "default-backend",
            "endpoint": "http://127.0.0.1:8000/v1",
            "api_key_env_var": "DEFRA_AGENT_TEST_MANIFEST_API_KEY",
            "max_concurrent": 2,
            "max_queue_depth": 100,
            "enabled": true,
            "models": ["mock-model"]
        }),
    )?;

    Ok(())
}
