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
    }
}

fn manifest_with_default_behavior() -> DesiredStateManifest {
    let mut manifest = empty_manifest("did:defra-agent:test");
    manifest.agent_principal.default_behavior_id = Some("did:defra-agent:test:default".to_string());
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

mod load_manifest_root {
    use std::fs;
    use tempfile::tempdir;
    use crate::desired_state::load::load_manifest_root;

    /// Write a minimal but fully valid manifest root: one principal with a
    /// `default_behavior_id` pointing to the single behavior in `agent-behaviors/default/`.
    fn write_minimal_root(root: &std::path::Path) {
        fs::write(
            root.join("agent-principal.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agent_did": "did:key:example",
                "default_behavior_id": "default",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();

        let behavior_dir = root.join("agent-behaviors").join("default");
        fs::create_dir_all(&behavior_dir).unwrap();
        fs::write(
            behavior_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "behavior_id": "default",
                "agent_did": "did:key:example",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn loads_minimal_valid_root() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        let (manifest, report) = load_manifest_root(tmp.path());
        assert!(report.ok, "expected ok, got errors: {:?}", report.errors);
        let manifest = manifest.unwrap();
        assert_eq!(manifest.agent_principal.agent_did, "did:key:example");
        assert_eq!(manifest.agent_behaviors.len(), 1);
        assert!(manifest.tasks.is_empty());
    }

    #[test]
    fn missing_principal_file_is_error() {
        let tmp = tempdir().unwrap();
        let (_, report) = load_manifest_root(tmp.path());
        assert!(!report.ok);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("agent-principal.json")),
            "got: {:?}",
            report.errors
        );
    }

    #[test]
    fn loads_behavior_with_sidecar_hydration() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        // Overwrite the default behavior with one that references a sidecar.
        let behavior_dir = tmp.path().join("agent-behaviors").join("default");
        fs::write(
            behavior_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "behavior_id": "default",
                "agent_did": "did:key:example",
                "system_prompt": "./system_prompt.md",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(behavior_dir.join("system_prompt.md"), "You are helpful.").unwrap();

        let (manifest, report) = load_manifest_root(tmp.path());
        assert!(report.ok, "errors: {:?}", report.errors);
        let behavior = &manifest.unwrap().agent_behaviors[0];
        assert_eq!(behavior.system_prompt.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn missing_sidecar_surfaces_error() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        // Overwrite the default behavior to reference a missing sidecar file.
        let behavior_dir = tmp.path().join("agent-behaviors").join("default");
        fs::write(
            behavior_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "behavior_id": "default",
                "agent_did": "did:key:example",
                "system_prompt": "./system_prompt.md",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, report) = load_manifest_root(tmp.path());
        assert!(!report.ok);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("sidecar path does not resolve")),
            "got: {:?}",
            report.errors
        );
    }
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
    task.prompt_template = "Schedule fired at {{ event.fired_at }} for {{ doc.foo }}.".to_string();
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
    task.prompt_template = "Run at {{ event.fired_at }} for {{ event.trigger_kind }}.".to_string();
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

    let bundle =
        export_bundle_from_manifest(&manifest, "local").expect("export bundle should be produced");
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

#[test]
fn hydrate_sidecar_replaces_dot_slash_path_with_file_contents() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("prompt.md"), "You are a helpful agent.").unwrap();

    let mut value = Some("./prompt.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_leaves_literal_string_untouched() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("You are a helpful agent.".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_ignores_absolute_path() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("/etc/hosts".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("/etc/hosts"));
}

#[test]
fn hydrate_sidecar_ignores_parent_relative_path() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("../elsewhere.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("../elsewhere.md"));
}

#[test]
fn hydrate_sidecar_errors_when_file_missing() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value = Some("./missing.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("sidecar path does not resolve"), "got: {err}");
    assert!(err.contains("missing.md"), "got: {err}");
}

#[test]
fn hydrate_sidecar_errors_on_non_utf8() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("bad.md"), &[0xff, 0xfe, 0xfd]).unwrap();
    let mut value = Some("./bad.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("not valid UTF-8"), "got: {err}");
}

#[test]
fn hydrate_sidecar_is_noop_on_none() {
    use tempfile::tempdir;
    use super::load::hydrate_sidecar;

    let dir = tempdir().unwrap();
    let mut value: Option<String> = None;
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert!(value.is_none());
}

mod load_per_doc_collection {
    use std::fs;
    use tempfile::tempdir;
    use crate::desired_state::load::load_per_doc_collection;
    use crate::desired_state::{DesiredAgentBehavior, HasUniqueId};
    use defra_agent::Collection;

    fn write_behavior_dir(root: &std::path::Path, handle: &str, behavior_id: &str) {
        let dir = root.join("agent-behaviors").join(handle);
        fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "behavior_id": behavior_id,
            "agent_did": "did:key:example",
            "enabled": true,
        });
        fs::write(dir.join("object.json"), serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    }

    #[test]
    fn loads_one_document_per_subdir() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "default", "default");
        write_behavior_dir(tmp.path(), "other", "other");

        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(result.len(), 2);
        let ids: Vec<&str> = result.iter().map(DesiredAgentBehavior::unique_id).collect();
        assert!(ids.contains(&"default") && ids.contains(&"other"));
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let tmp = tempdir().unwrap();
        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty());
        assert!(result.is_empty());
    }

    #[test]
    fn missing_object_json_is_error() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("agent-behaviors").join("default")).unwrap();
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("is missing object.json"), "got: {:?}", errors);
    }

    #[test]
    fn handle_mismatch_is_error() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "on-disk-name", "id-inside-json");
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not match behavior_id"), "got: {:?}", errors);
        assert!(errors[0].contains("on-disk-name"));
        assert!(errors[0].contains("id-inside-json"));
    }

    #[test]
    fn duplicate_unique_id_is_error() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "alpha", "shared");
        write_behavior_dir(tmp.path(), "beta", "shared");
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(
            errors.iter().any(|e| {
                e.contains("duplicate behavior_id 'shared'")
                    && e.contains("alpha")
                    && e.contains("beta")
            }),
            "got: {:?}",
            errors
        );
    }

    #[test]
    fn unknown_sibling_files_are_ignored() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "default", "default");
        fs::write(
            tmp.path().join("agent-behaviors").join("default").join("README.md"),
            "notes",
        )
        .unwrap();
        fs::write(
            tmp.path().join("agent-behaviors").join("default").join(".DS_Store"),
            "",
        )
        .unwrap();
        let mut errors = Vec::new();
        let result: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn non_directory_collection_path_is_error() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("agent-behaviors"), "not a dir").unwrap();
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert!(
            errors.iter().any(|e| e.contains("is not a directory")),
            "got: {:?}",
            errors
        );
    }
}

pub(super) mod write_manifest_root {
    use std::fs;
    use tempfile::tempdir;

    use crate::desired_state::{
        write_manifest_root, DesiredAgentBehavior, DesiredAgentPrincipal,
        DesiredStateManifest, DesiredTask,
    };

    pub(in crate::desired_state::tests) fn minimal_manifest() -> DesiredStateManifest {
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:example".to_string(),
                display_name: None,
                default_behavior_id: Some("default".to_string()),
                enabled: true,
            },
            agent_behaviors: vec![DesiredAgentBehavior {
                behavior_id: "default".to_string(),
                agent_did: "did:key:example".to_string(),
                display_name: None,
                system_prompt: Some("You are helpful.".to_string()),
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: true,
            }],
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            tasks: vec![DesiredTask {
                task_id: "seed-health".to_string(),
                name: "Seed fleet health".to_string(),
                description: None,
                behavior_id: "default".to_string(),
                prompt_template: "Check the fleet.".to_string(),
                enabled: true,
                output_schema_ref: None,
            }],
            schedules: Vec::new(),
        }
    }

    #[test]
    fn writes_principal_and_per_doc_dirs_with_sidecars() {
        let tmp = tempdir().unwrap();
        write_manifest_root(tmp.path(), &minimal_manifest(), false).unwrap();

        assert!(tmp.path().join("agent-principal.json").is_file());

        let behavior_object = tmp.path().join("agent-behaviors/default/object.json");
        assert!(behavior_object.is_file());
        let behavior_sidecar = tmp.path().join("agent-behaviors/default/system_prompt.md");
        assert!(behavior_sidecar.is_file());
        assert_eq!(fs::read_to_string(&behavior_sidecar).unwrap(), "You are helpful.");
        let behavior_body: serde_json::Value =
            serde_json::from_slice(&fs::read(&behavior_object).unwrap()).unwrap();
        assert_eq!(
            behavior_body.get("system_prompt").and_then(|v| v.as_str()),
            Some("./system_prompt.md")
        );

        let task_object = tmp.path().join("tasks/seed-health/object.json");
        assert!(task_object.is_file());
        let task_sidecar = tmp.path().join("tasks/seed-health/prompt.md");
        assert!(task_sidecar.is_file());
        assert_eq!(fs::read_to_string(&task_sidecar).unwrap(), "Check the fleet.");
        let task_body: serde_json::Value =
            serde_json::from_slice(&fs::read(&task_object).unwrap()).unwrap();
        assert_eq!(
            task_body.get("prompt_template").and_then(|v| v.as_str()),
            Some("./prompt.md")
        );
    }

    #[test]
    fn none_system_prompt_omits_sidecar_and_field() {
        let tmp = tempdir().unwrap();
        let mut m = minimal_manifest();
        m.agent_behaviors[0].system_prompt = None;
        write_manifest_root(tmp.path(), &m, false).unwrap();

        let sidecar = tmp.path().join("agent-behaviors/default/system_prompt.md");
        assert!(!sidecar.exists());
        let body: serde_json::Value = serde_json::from_slice(
            &fs::read(tmp.path().join("agent-behaviors/default/object.json")).unwrap(),
        )
        .unwrap();
        assert!(body.get("system_prompt").is_none());
    }

    #[test]
    fn rejects_behavior_with_unsafe_id() {
        let tmp = tempdir().unwrap();
        let mut m = minimal_manifest();
        m.agent_behaviors[0].behavior_id = "bad/id".to_string();
        let err = write_manifest_root(tmp.path(), &m, false).unwrap_err();
        assert!(err.contains("filesystem-unsafe"), "got: {err}");
    }

    #[test]
    fn force_refuses_dir_without_agent_principal() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("random.txt"), "this is not a manifest root").unwrap();
        let err = write_manifest_root(tmp.path(), &minimal_manifest(), true).unwrap_err();
        assert!(
            err.contains("does not contain agent-principal.json"),
            "got: {err}"
        );
        // File was not deleted.
        assert!(tmp.path().join("random.txt").exists());
    }
}

mod write_manifest_root_safe_id {
    use crate::desired_state::write::check_filesystem_safe_id;

    #[test]
    fn accepts_ordinary_ids() {
        assert!(check_filesystem_safe_id("default").is_ok());
        assert!(check_filesystem_safe_id("workstation-1").is_ok());
        assert!(check_filesystem_safe_id("seed_fleet_health").is_ok());
    }

    #[test]
    fn rejects_forward_slash() {
        let err = check_filesystem_safe_id("foo/bar").unwrap_err();
        assert!(err.contains("filesystem-unsafe"), "got: {err}");
    }

    #[test]
    fn rejects_backslash_colon_and_null() {
        for bad in ["a\\b", "a:b", "a\0b"] {
            assert!(check_filesystem_safe_id(bad).is_err(), "should reject '{bad}'");
        }
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(check_filesystem_safe_id(".").is_err());
        assert!(check_filesystem_safe_id("..").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(check_filesystem_safe_id("").is_err());
    }
}
