use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::convert::{
    export_bundle_from_manifest, manifest_from_export_bundle, tool_service_registry_from_live_value,
};
use super::diff::{diff_collection, diff_manifests};
use super::validate::validate_manifest;
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
        event_triggers: Vec::new(),
    }
}

fn manifest_with_default_behavior() -> DesiredStateManifest {
    let mut manifest = empty_manifest("did:defra-agent:test");
    manifest.agent_principal.default_behavior_id =
        Some("did:defra-agent:test:default".to_string());
    manifest.agent_behaviors.push(DesiredAgentBehavior {
        behavior_id: "did:defra-agent:test:default".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        display_name: None,
        system_prompt: None,
        backend_id: None,
        model_name: None,
        tool_selection_id: None,
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
    });
    manifest
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
fn load_manifest_root_loads_event_triggers() {
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
            "description": null,
            "behavior_id": "did:defra-agent:test:default",
            "prompt_template": "Summarize the unread emails.",
            "enabled": true,
            "output_schema_ref": null
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("event_triggers.json"),
        r#"[{
            "trigger_id": "new-customer-greet",
            "task_id": "summarize-inbox",
            "source_collection": "CustomerSignup",
            "event_kind": "created",
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

    assert_eq!(report.counts.event_triggers, 1);
    assert_eq!(manifest.event_triggers.len(), 1);
    assert_eq!(manifest.event_triggers[0].trigger_id, "new-customer-greet");
    assert_eq!(
        manifest.event_triggers[0].source_collection,
        "CustomerSignup"
    );
    assert_eq!(manifest.event_triggers[0].event_kind, "created");
    assert!(manifest.event_triggers[0].enabled);
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

fn validation_errors(manifest: &DesiredStateManifest) -> Vec<String> {
    let mut errors = Vec::new();
    validate_manifest(manifest, &mut errors);
    errors
}

#[test]
fn validate_rejects_empty_task_id() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("");
    task.task_id = String::new();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("empty task_id")),
        "expected empty task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_empty_task_behavior_id() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.behavior_id = String::new();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("summarize-inbox") && message.contains("behavior_id")),
        "expected empty behavior_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_task_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest.tasks.push(sample_task("summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate task_id") && message.contains("summarize-inbox")
        }),
        "expected duplicate task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_empty_schedule_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("", "summarize-inbox");
    schedule.schedule_id = String::new();
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("empty schedule_id")),
        "expected empty schedule_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_schedule_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate schedule_id") && message.contains("hourly")
        }),
        "expected duplicate schedule_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_interval_zero_or_negative() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("hourly", "summarize-inbox");
    schedule.interval_secs = 0;
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("hourly") && message.contains("interval_secs")),
        "expected interval_secs >= 1 rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_unknown_concurrency() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("hourly", "summarize-inbox");
    schedule.concurrency = "everything-everywhere".to_string();
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("concurrency")
                && message.contains("everything-everywhere")
        }),
        "expected unknown concurrency rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_task_unknown_behavior() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.behavior_id = "did:defra-agent:test:missing".to_string();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("summarize-inbox")
                && message.contains("missing")
                && message.contains("behavior_id")
        }),
        "expected missing behavior_id reference rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_task_template_referencing_doc_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template =
        "Schedule fired at {{ event.fired_at }} for {{ doc.foo }}.".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("forbidden scope")
                && message.contains("doc")
                && message.contains("event.*")
        }),
        "expected schedule-scope rejection for doc.*, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_task_template_referencing_args_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ args.target }}".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("forbidden scope")
                && message.contains("args")
        }),
        "expected schedule-scope rejection for args.*, got {errors:?}"
    );
}

#[test]
fn validate_accepts_schedule_task_template_using_only_event_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template =
        "Run at {{ event.fired_at }} for {{ event.trigger_kind }}.".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("forbidden scope")),
        "expected no schedule-scope rejections, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_unknown_task() {
    let mut manifest = manifest_with_default_behavior();
    manifest
        .schedules
        .push(sample_schedule("hourly", "missing-task"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("missing-task")
                && message.contains("task_id")
        }),
        "expected missing task_id reference rejection, got {errors:?}"
    );
}

#[test]
fn export_bundle_round_trip_preserves_tasks_and_schedules() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("beta-task"));
    manifest.tasks.push(sample_task("alpha-task"));
    manifest
        .schedules
        .push(sample_schedule("beta-hourly", "beta-task"));
    manifest
        .schedules
        .push(sample_schedule("alpha-hourly", "alpha-task"));

    let bundle = export_bundle_from_manifest(&manifest, "local")
        .expect("export bundle should be produced");
    assert_eq!(bundle.as_bundle().tasks.len(), 2);
    assert_eq!(bundle.as_bundle().schedules.len(), 2);

    let round_tripped = manifest_from_export_bundle(bundle.as_bundle())
        .expect("manifest should parse back from bundle");

    // `manifest_from_export_bundle` normalizes (sorts by id).
    let task_ids: Vec<_> = round_tripped
        .tasks
        .iter()
        .map(|task| task.task_id.as_str())
        .collect();
    assert_eq!(task_ids, vec!["alpha-task", "beta-task"]);

    let schedule_ids: Vec<_> = round_tripped
        .schedules
        .iter()
        .map(|schedule| schedule.schedule_id.as_str())
        .collect();
    assert_eq!(schedule_ids, vec!["alpha-hourly", "beta-hourly"]);

    let mut expected_tasks = manifest.tasks.clone();
    expected_tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    assert_eq!(round_tripped.tasks, expected_tasks);

    let mut expected_schedules = manifest.schedules.clone();
    expected_schedules.sort_by(|left, right| left.schedule_id.cmp(&right.schedule_id));
    assert_eq!(round_tripped.schedules, expected_schedules);
}
