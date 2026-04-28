mod support;
use support::*;

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read all `object.json` files from subdirectories of a collection dir.
/// Returns an empty Vec if the directory does not exist.
fn read_per_doc_collection(root: &Path, dir_name: &str) -> Result<Vec<Value>> {
    let dir = root.join(dir_name);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut docs = vec![];
    for entry in
        fs::read_dir(&dir).with_context(|| format!("reading collection dir {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("iterating {}", dir.display()))?;
        let object_path = entry.path().join("object.json");
        if object_path.exists() {
            docs.push(read_json_file(&object_path)?);
        }
    }
    Ok(docs)
}

/// Run `defra-agent config export --root <root>` and return the stdout
/// confirmation string. Errors if the command exits non-zero.
fn run_config_export(home_dir: &Path, root: &Path, extra_args: &[&str]) -> Result<String> {
    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("export root path is not UTF-8"))?;
    let mut args = vec!["config", "export", "--root", root_str];
    args.extend_from_slice(extra_args);
    run_cli_text(home_dir, &args)
}

/// Run `defra-agent config apply --root <root>` and return the JSON report.
fn run_config_apply(home_dir: &Path, root: &Path) -> Result<Value> {
    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("apply root path is not UTF-8"))?;
    run_cli_json(home_dir, &["config", "apply", "--root", root_str])
}

/// Write a minimal manifest root with a single agent, behavior, tool-selection,
/// and inference-backend, all using filesystem-safe IDs (no colons).
/// Returns `(agent_did, behavior_id, selection_id, backend_id)`.
fn write_simple_manifest_root(
    root: &Path,
    agent_name: &str,
    backend_endpoint: &str,
    model_name: &str,
) -> Result<(String, String, String, String)> {
    let agent_did = format!("did-{agent_name}"); // no colon separators
    let behavior_id = format!("{agent_name}-default");
    let selection_id = format!("{agent_name}-tools");
    let backend_id = format!("{agent_name}-backend");

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did,
            "display_name": agent_name,
            "default_behavior_id": behavior_id,
            "enabled": true
        }),
    )?;
    {
        let dir = root.join("agent-behaviors").join(&behavior_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "behavior_id": behavior_id,
                "agent_did": agent_did,
                "display_name": "Default",
                "system_prompt": "Keep responses short.",
                "backend_id": backend_id,
                "model_name": model_name,
                "tool_selection_id": selection_id,
                "enabled": true
            }),
        )?;
    }
    {
        let dir = root.join("tool-selections").join(&selection_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "selection_id": selection_id,
                "agent_did": agent_did,
                "display_name": "Standard",
                "enable_file_tools": false,
                "file_tools_mode": "ReadOnly",
                "enable_bash": false,
                "bash_mode": "ReadOnly",
                "cli_tool_names": [],
                "enable_meta_tools": false,
                "delegate_to": []
            }),
        )?;
    }
    {
        let dir = root.join("inference-backends").join(&backend_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "backend_id": backend_id,
                "name": backend_id,
                "endpoint": backend_endpoint,
                "api_key_env_var": null,
                "max_concurrent": 2,
                "max_queue_depth": 100,
                "enabled": true,
                "models": [model_name]
            }),
        )?;
    }

    Ok((agent_did, behavior_id, selection_id, backend_id))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test that `config import` correctly imports a JSON bundle, rejects a second
/// import without `--override`, and accepts one with `--override`.
///
/// This test builds its bundle fixture inline (not from `config export --root`)
/// so that it exercises the `config import` mechanics without being affected
/// by the filesystem-safe-ID requirement that `config export --root` enforces.
/// The `config export --root` → `config apply --root` roundtrip is covered by
/// `config_export_apply_round_trips_with_extra_collections` below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_import_round_trips_and_requires_override() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home = tempdir.path().join("source-home");
    let target_home = tempdir.path().join("target-home");
    fs::create_dir_all(&source_home)?;
    fs::create_dir_all(&target_home)?;

    let agent_name = format!("cli-import-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let default_behavior_id = "default".to_string();
    let tool_selection_id = format!("{default_behavior_id}-tools");
    let backend_id = format!("{agent_name}-backend");
    let export_path = tempdir.path().join("agent-config.json");

    // Build an inline JSON bundle representing a freshly initialised agent.
    let bundle = serde_json::json!({
        "format": "defra-agent-config/v2",
        "agent_did": agent_did,
        "exported_at": "2026-01-01T00:00:00Z",
        "access_mode": "local",
        "agent_principal": {
            "agent_did": agent_did,
            "display_name": agent_name,
            "default_behavior_id": default_behavior_id,
            "enabled": true
        },
        "agent_behaviors": [{
            "behavior_id": default_behavior_id,
            "agent_did": agent_did,
            "display_name": "Default",
            "system_prompt": "Keep responses short.",
            "backend_id": backend_id,
            "model_name": "mock-model",
            "tool_selection_id": tool_selection_id,
            "enabled": true
        }],
        "tool_selections": [{
            "selection_id": tool_selection_id,
            "agent_did": agent_did,
            "display_name": "Standard",
            "enable_file_tools": false,
            "enable_bash": false,
            "enable_meta_tools": false
        }],
        "inference_backends": [{
            "backend_id": backend_id,
            "name": backend_id,
            "endpoint": "http://127.0.0.1:8999/v1",
            "enabled": true,
            "models": ["mock-model"]
        }],
        "inference_profiles": [],
        "tool_service_registries": [],
        "tasks": [],
        "schedules": []
    });

    fs::write(&export_path, serde_json::to_vec_pretty(&bundle)?)
        .context("writing inline bundle fixture")?;

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

    // A second import without --override must fail with guidance.
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

    // With --override the import must succeed.
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

/// Test that `config export --root` writes a manifest root that faithfully
/// captures tool services, tasks, and schedules added via `config import`,
/// and that `config apply --root` can reproduce those docs in a fresh DB.
///
/// Uses filesystem-safe IDs (no colons) so that `config export --root`
/// does not reject the behavior/selection handles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_apply_round_trips_with_extra_collections() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home = tempdir.path().join("source-home");
    let target_home = tempdir.path().join("target-home");
    let initial_root = tempdir.path().join("initial-root");
    let export_root = tempdir.path().join("export-root");
    let reapply_root = tempdir.path().join("reapply-root");
    fs::create_dir_all(&source_home)?;
    fs::create_dir_all(&target_home)?;

    let agent_name = format!("cli-export-apply-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let service_id = format!("ops-mcp-{}", Uuid::new_v4().simple());
    let task_id = format!("nightly-audit-{}", Uuid::new_v4().simple());
    let schedule_id = format!("{task_id}-hourly");

    // Set up the source DB via `config apply --root` using simple, safe IDs.
    let (agent_did, behavior_id, _selection_id, _backend_id) = write_simple_manifest_root(
        &initial_root,
        &agent_name,
        mock_endpoint.endpoint(),
        &model_name,
    )?;

    // Add a tool service, task, and schedule to the initial root.
    {
        let dir = initial_root.join("tool-services").join(&service_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "service_id": service_id,
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
        let dir = initial_root.join("tasks").join(&task_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "task_id": task_id,
                "name": "Nightly Audit",
                "description": null,
                "behavior_id": behavior_id,
                "prompt_template": "Audit the fleet state and summarize drift.",
                "enabled": false,
                "output_schema_ref": null
            }),
        )?;
    }
    {
        let dir = initial_root.join("schedules").join(&schedule_id);
        fs::create_dir_all(&dir)?;
        write_json_file(
            &dir.join("object.json"),
            &serde_json::json!({
                "schedule_id": schedule_id,
                "task_id": task_id,
                "interval_secs": 3600,
                "enabled": false,
                "concurrency": "serial"
            }),
        )?;
    }

    // Apply the manifest root to the source DB.
    let applied = run_config_apply(&source_home, &initial_root)?;
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

    // Export the source DB to a new manifest root.
    let export_stdout =
        run_config_export(&source_home, &export_root, &["--agent-did", &agent_did])?;
    assert!(
        export_stdout.contains("wrote manifest root"),
        "expected export confirmation, got: {export_stdout}"
    );

    // Verify the export root contains the expected collections.
    let principal = read_json_file(&export_root.join("agent-principal.json"))?;
    assert_eq!(
        principal.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );

    let tool_services = read_per_doc_collection(&export_root, "tool-services")?;
    assert_eq!(
        tool_services
            .iter()
            .find(|s| s.get("service_id").and_then(Value::as_str) == Some(service_id.as_str()))
            .and_then(|s| s.get("service_id"))
            .and_then(Value::as_str),
        Some(service_id.as_str()),
        "expected tool service {service_id} in export root"
    );
    // Runtime-only fields should be absent.
    let ts_object = read_json_file(
        &export_root
            .join("tool-services")
            .join(&service_id)
            .join("object.json"),
    )?;
    assert!(
        ts_object.get("status").is_none(),
        "tool-service export should omit runtime status: {ts_object}"
    );
    assert!(
        ts_object.get("tools").is_none(),
        "tool-service export should omit discovered tools: {ts_object}"
    );

    let tasks = read_per_doc_collection(&export_root, "tasks")?;
    let task_doc = tasks
        .iter()
        .find(|t| t.get("task_id").and_then(Value::as_str) == Some(task_id.as_str()))
        .ok_or_else(|| anyhow!("task {task_id} not found in export root"))?;
    // The prompt_template may be spilled to a sidecar file.  Accept either the
    // sidecar reference ("./prompt.md") or the inline value.
    let prompt_template = task_doc
        .get("prompt_template")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("task {task_id} missing prompt_template field"))?;
    if prompt_template == "./prompt.md" {
        let sidecar_path = export_root.join("tasks").join(&task_id).join("prompt.md");
        let sidecar = fs::read_to_string(&sidecar_path)
            .with_context(|| format!("reading sidecar {}", sidecar_path.display()))?;
        assert_eq!(
            sidecar.trim(),
            "Audit the fleet state and summarize drift.",
            "sidecar content should match original prompt_template"
        );
    } else {
        assert_eq!(
            prompt_template, "Audit the fleet state and summarize drift.",
            "inline prompt_template should match original"
        );
    }

    let schedules = read_per_doc_collection(&export_root, "schedules")?;
    assert_eq!(
        schedules
            .iter()
            .find(|s| s.get("schedule_id").and_then(Value::as_str) == Some(schedule_id.as_str()))
            .and_then(|s| s.get("schedule_id"))
            .and_then(Value::as_str),
        Some(schedule_id.as_str()),
        "expected schedule {schedule_id} in export root"
    );

    // Apply the exported root to a fresh target DB and verify convergence.
    let reapplied = run_config_apply(&target_home, &export_root)?;
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

    // Re-export from the target and verify the data survived the apply.
    let reexport_stdout =
        run_config_export(&target_home, &reapply_root, &["--agent-did", &agent_did])?;
    assert!(
        reexport_stdout.contains("wrote manifest root"),
        "expected reexport confirmation, got: {reexport_stdout}"
    );

    let reexported_tasks = read_per_doc_collection(&reapply_root, "tasks")?;
    assert_eq!(
        reexported_tasks
            .iter()
            .find(|t| t.get("task_id").and_then(Value::as_str) == Some(task_id.as_str()))
            .and_then(|t| t.get("task_id"))
            .and_then(Value::as_str),
        Some(task_id.as_str()),
        "expected task {task_id} in re-exported root"
    );

    let reexported_schedules = read_per_doc_collection(&reapply_root, "schedules")?;
    assert_eq!(
        reexported_schedules
            .iter()
            .find(|s| s.get("schedule_id").and_then(Value::as_str) == Some(schedule_id.as_str()))
            .and_then(|s| s.get("schedule_id"))
            .and_then(Value::as_str),
        Some(schedule_id.as_str()),
        "expected schedule {schedule_id} in re-exported root"
    );

    Ok(())
}
