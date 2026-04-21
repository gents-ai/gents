use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::convert::tool_service_registry_from_live_value;
use super::diff::{diff_collection, diff_manifests};
use super::*;

fn empty_manifest(agent_did: &str) -> DesiredStateManifest {
    DesiredStateManifest {
        agent_principal: DesiredAgentPrincipal {
            agent_did: agent_did.to_string(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        },
        agent_behaviors: Vec::new(),
        tool_selections: Vec::new(),
        inference_backends: Vec::new(),
        inference_profiles: Vec::new(),
        tool_service_registries: Vec::new(),
        tasks: Vec::new(),
        schedules: Vec::new(),
    }
}

fn sample_task(task_id: &str) -> DesiredTask {
    DesiredTask {
        task_id: task_id.to_string(),
        name: "Sample task".to_string(),
        description: None,
        behavior_id: "did:defra-agent:test:default".to_string(),
        prompt_template: "Do the thing.".to_string(),
        enabled: true,
        output_schema_ref: None,
    }
}

fn sample_schedule(schedule_id: &str, task_id: &str) -> DesiredSchedule {
    DesiredSchedule {
        schedule_id: schedule_id.to_string(),
        task_id: task_id.to_string(),
        interval_secs: 3600,
        enabled: true,
        concurrency: "serial".to_string(),
    }
}

#[test]
fn desired_tool_service_registry_normalizes_address_storage_fields() {
    let service: DesiredToolServiceRegistry = serde_json::from_value(json!({
        "service_id": "observability-mcp",
        "display_name": "Observability",
        "description": null,
        "hostname": null,
        "tailscale_ip": " 100.64.0.10 ",
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": "mcp"
    }))
    .expect("desired tool service should deserialize");

    assert_eq!(service.hostname.as_deref(), Some(""));
    assert_eq!(service.tailscale_ip.as_deref(), Some("100.64.0.10"));
    assert_eq!(service.lan_ip.as_deref(), Some(""));
    assert_eq!(service.mcp_path.as_deref(), Some("/mcp"));
}

#[test]
fn live_tool_service_registry_preserves_null_storage_for_diff() {
    let service = tool_service_registry_from_live_value(&json!({
        "service_id": "observability-mcp",
        "hostname": null,
        "tailscale_ip": null,
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": null
    }))
    .expect("live tool service should parse");

    assert_eq!(service.hostname, None);
    assert_eq!(service.tailscale_ip, None);
    assert_eq!(service.lan_ip, None);
    assert_eq!(service.mcp_path, None);
}

#[test]
fn diff_marks_live_null_tool_service_storage_for_update() {
    let desired: DesiredToolServiceRegistry = serde_json::from_value(json!({
        "service_id": "observability-mcp",
        "hostname": "studio-1",
        "mcp_port": 9201
    }))
    .expect("desired tool service should deserialize");
    let live = tool_service_registry_from_live_value(&json!({
        "service_id": "observability-mcp",
        "hostname": "studio-1",
        "tailscale_ip": null,
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": null
    }))
    .expect("live tool service should parse");

    let diff = diff_collection(
        vec![(desired.service_id.clone(), &desired)],
        vec![(live.service_id.clone(), &live)],
    );

    assert_eq!(diff.update, vec!["observability-mcp"]);
    assert!(diff.unchanged.is_empty());
}

#[test]
fn deprecated_backend_capability_fields_are_ignored_for_diff_equality() {
    let with_deprecated: DesiredInferenceBackend = serde_json::from_value(json!({
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:11434/v1",
        "api_key": null,
        "api_key_env_var": null,
        "max_concurrent": 1,
        "max_queue_depth": 100,
        "enabled": true,
        "supports_tool_calls": false,
        "supports_streaming": false,
        "supports_structured_outputs": true,
        "supports_json_schema": true,
        "context_window": 32768,
        "max_output_tokens": 4096,
        "models": ["test-model"]
    }))
    .expect("deprecated fields should deserialize");

    let current: DesiredInferenceBackend = serde_json::from_value(json!({
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:11434/v1",
        "api_key": null,
        "api_key_env_var": null,
        "max_concurrent": 1,
        "max_queue_depth": 100,
        "enabled": true,
        "models": ["test-model"]
    }))
    .expect("current fields should deserialize");

    assert_eq!(with_deprecated, current);
    assert_eq!(
        serde_json::to_value(with_deprecated).unwrap(),
        serde_json::to_value(current).unwrap()
    );
}

#[test]
fn load_manifest_root_loads_tasks_and_schedules() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path();

    fs::write(
        root.join("agent-principal.json"),
        r#"{
            "agent_did": "did:defra-agent:test",
            "display_name": "Test",
            "default_behavior_id": "did:defra-agent:test:default",
            "enabled": true
        }"#,
    )
    .unwrap();
    fs::write(
        root.join("agent-behaviors.json"),
        r#"[{
            "behavior_id": "did:defra-agent:test:default",
            "agent_did": "did:defra-agent:test",
            "display_name": "Default",
            "system_prompt": null,
            "backend_id": "local",
            "model_name": "test-model",
            "tool_selection_id": "tools",
            "inference_profile_id": null,
            "compaction_strategy": null,
            "compaction_threshold": null,
            "enabled": true
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("tool-selections.json"),
        r#"[{
            "selection_id": "tools",
            "agent_did": "did:defra-agent:test",
            "display_name": "Tools",
            "enable_file_tools": false,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "Off",
            "cli_tool_names": [],
            "enable_meta_tools": true,
            "delegate_to": []
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("inference-backends.json"),
        r#"[{
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "models": ["test-model"]
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("tasks.json"),
        r#"[{
            "task_id": "summarize-inbox",
            "name": "Summarize inbox",
            "description": "Produce a short summary of unread mail.",
            "behavior_id": "did:defra-agent:test:default",
            "prompt_template": "Summarize the unread emails.",
            "enabled": true,
            "output_schema_ref": null
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("schedules.json"),
        r#"[{
            "schedule_id": "summarize-inbox-hourly",
            "task_id": "summarize-inbox",
            "interval_secs": 3600,
            "enabled": true,
            "concurrency": "serial"
        }]"#,
    )
    .unwrap();

    let (manifest, report) = load_manifest_root(root);
    assert!(
        report.ok,
        "expected valid manifest, got {:?}",
        report.errors
    );
    let manifest = manifest.expect("manifest should load");

    assert_eq!(manifest.tasks.len(), 1);
    assert_eq!(manifest.tasks[0].task_id, "summarize-inbox");
    assert_eq!(manifest.tasks[0].behavior_id, "did:defra-agent:test:default");

    assert_eq!(manifest.schedules.len(), 1);
    assert_eq!(manifest.schedules[0].schedule_id, "summarize-inbox-hourly");
    assert_eq!(manifest.schedules[0].task_id, "summarize-inbox");
    assert_eq!(manifest.schedules[0].interval_secs, 3600);
    assert_eq!(manifest.schedules[0].concurrency, "serial");

    assert_eq!(report.counts.tasks, 1);
    assert_eq!(report.counts.schedules, 1);
}

#[test]
fn validate_manifest_accepts_deprecated_backend_capability_fields() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path();

    fs::write(
        root.join("agent-principal.json"),
        r#"{
            "agent_did": "did:defra-agent:test",
            "display_name": "Test",
            "default_behavior_id": "did:defra-agent:test:default",
            "enabled": true
        }"#,
    )
    .unwrap();
    fs::write(
        root.join("agent-behaviors.json"),
        r#"[{
            "behavior_id": "did:defra-agent:test:default",
            "agent_did": "did:defra-agent:test",
            "display_name": "Default",
            "system_prompt": null,
            "backend_id": "local",
            "model_name": "test-model",
            "tool_selection_id": "tools",
            "inference_profile_id": null,
            "compaction_strategy": null,
            "compaction_threshold": null,
            "enabled": true
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("tool-selections.json"),
        r#"[{
            "selection_id": "tools",
            "agent_did": "did:defra-agent:test",
            "display_name": "Tools",
            "enable_file_tools": false,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "Off",
            "cli_tool_names": [],
            "enable_meta_tools": true,
            "delegate_to": []
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("inference-backends.json"),
        r#"[{
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "supports_tool_calls": true,
            "supports_streaming": true,
            "supports_structured_outputs": false,
            "supports_json_schema": false,
            "models": ["test-model"]
        }]"#,
    )
    .unwrap();

    let report = validate_manifest_root(root);
    assert!(
        report.ok,
        "expected valid manifest, got {:?}",
        report.errors
    );
}

#[test]
fn diff_manifests_creates_task_when_live_is_empty() {
    let mut desired = empty_manifest("did:defra-agent:test");
    desired.tasks.push(sample_task("summarize-inbox"));
    let live = empty_manifest("did:defra-agent:test");

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &desired,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(report.collections.tasks.create, vec!["summarize-inbox"]);
    assert!(report.collections.tasks.update.is_empty());
    assert!(report.collections.tasks.unchanged.is_empty());
    assert!(report.collections.tasks.live_only.is_empty());
    assert_eq!(report.counts.tasks.create, 1);
    assert_eq!(report.counts.tasks.update, 0);
    assert_eq!(report.counts.tasks.unchanged, 0);
    assert_eq!(report.counts.tasks.live_only, 0);
}

#[test]
fn diff_manifests_creates_schedule_when_live_is_empty() {
    let mut desired = empty_manifest("did:defra-agent:test");
    desired
        .schedules
        .push(sample_schedule("summarize-inbox-hourly", "summarize-inbox"));
    let live = empty_manifest("did:defra-agent:test");

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &desired,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(
        report.collections.schedules.create,
        vec!["summarize-inbox-hourly"]
    );
    assert!(report.collections.schedules.update.is_empty());
    assert!(report.collections.schedules.unchanged.is_empty());
    assert!(report.collections.schedules.live_only.is_empty());
    assert_eq!(report.counts.schedules.create, 1);
    assert_eq!(report.counts.schedules.update, 0);
    assert_eq!(report.counts.schedules.unchanged, 0);
    assert_eq!(report.counts.schedules.live_only, 0);
}

#[test]
fn diff_manifests_marks_task_update_when_prompt_changes() {
    let mut desired = empty_manifest("did:defra-agent:test");
    let mut desired_task = sample_task("summarize-inbox");
    desired_task.prompt_template = "New prompt body.".to_string();
    desired.tasks.push(desired_task);

    let mut live = empty_manifest("did:defra-agent:test");
    live.tasks.push(sample_task("summarize-inbox"));

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &desired,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(report.collections.tasks.update, vec!["summarize-inbox"]);
    assert!(report.collections.tasks.create.is_empty());
    assert!(report.collections.tasks.unchanged.is_empty());
    assert_eq!(report.counts.tasks.update, 1);
}

#[test]
fn diff_manifests_marks_schedule_update_when_interval_changes() {
    let mut desired = empty_manifest("did:defra-agent:test");
    let mut desired_schedule = sample_schedule("summarize-inbox-hourly", "summarize-inbox");
    desired_schedule.interval_secs = 7200;
    desired.schedules.push(desired_schedule);

    let mut live = empty_manifest("did:defra-agent:test");
    live.schedules
        .push(sample_schedule("summarize-inbox-hourly", "summarize-inbox"));

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &desired,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(
        report.collections.schedules.update,
        vec!["summarize-inbox-hourly"]
    );
    assert!(report.collections.schedules.create.is_empty());
    assert!(report.collections.schedules.unchanged.is_empty());
    assert_eq!(report.counts.schedules.update, 1);
}
