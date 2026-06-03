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
    manifest.agent_principal.default_behavior_id = Some("default".to_string());
    manifest.agent_behaviors.push(DesiredAgentBehavior {
        behavior_id: "default".to_string(),
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
        behavior_id: "default".to_string(),
        prompt_template: "Do the thing.".to_string(),
        enabled: true,
        output_schema_ref: None,
    }
}

fn sample_tool_selection(selection_id: &str) -> DesiredToolSelection {
    DesiredToolSelection {
        selection_id: selection_id.to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        display_name: None,
        enable_file_tools: false,
        file_tools_mode: "ReadOnly".to_string(),
        file_tool_root: None,
        enable_bash: false,
        bash_mode: "ReadOnly".to_string(),
        command_execution_policy: None,
        command_allowed_argv_prefixes: Vec::new(),
        command_forbidden_argv_prefixes: Vec::new(),
        command_network_mode: None,
        cli_tool_names: Vec::new(),
        enable_meta_tools: true,
        allowed_mcp_service_ids: Vec::new(),
        delegate_to: Vec::new(),
        backgroundable_tool_names: Vec::new(),
        enable_defra_query: true,
        defra_query_collections: Vec::new(),
        subagent_targets: None,
        subagent_spawn_enabled: None,
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

fn sample_event_trigger() -> DesiredEventTrigger {
    DesiredEventTrigger {
        trigger_id: "new-customer-greet".into(),
        task_id: "summarize-inbox".into(),
        source_collection: "CustomerSignup".into(),
        event_kind: "created".into(),
        filter: None,
        enabled: true,
        concurrency: "serial".into(),
    }
}

fn empty_manifest_with_event_trigger(t: DesiredEventTrigger) -> DesiredStateManifest {
    let mut m = empty_manifest("did:defra-agent:test");
    m.event_triggers.push(t);
    m
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
    assert!(!service.send_agent_did);
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
    assert!(!service.send_agent_did);
}

#[test]
fn tool_service_registry_round_trip_preserves_send_agent_did() {
    let mut manifest = empty_manifest("did:defra-agent:test");
    manifest
        .tool_service_registries
        .push(DesiredToolServiceRegistry {
            service_id: "identity-aware-mcp".to_string(),
            display_name: Some("Identity-aware MCP".to_string()),
            description: None,
            hostname: Some("studio-1".to_string()),
            tailscale_ip: Some(String::new()),
            lan_ip: Some(String::new()),
            mcp_port: Some(9201),
            mcp_path: Some("/mcp".to_string()),
            send_agent_did: true,
        });

    let bundle =
        export_bundle_from_manifest(&manifest, "local").expect("export bundle should be produced");
    assert_eq!(
        bundle.as_bundle().tool_service_registries[0]["send_agent_did"],
        json!(true)
    );

    let round_tripped = manifest_from_export_bundle(bundle.as_bundle())
        .expect("manifest should parse back from bundle");
    assert!(round_tripped.tool_service_registries[0].send_agent_did);
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
fn round_trip_load_write_load_is_identity() {
    use crate::desired_state::{load::load_manifest_root, write_manifest_root};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let original = self::write_manifest_root::minimal_manifest();

    write_manifest_root(tmp.path(), &original, false).unwrap();
    let (loaded, report) = load_manifest_root(tmp.path());
    assert!(report.ok, "errors: {:?}", report.errors);
    let loaded = loaded.unwrap();

    assert_eq!(loaded.agent_principal, original.agent_principal);
    assert_eq!(loaded.agent_behaviors, original.agent_behaviors);
    assert_eq!(loaded.tool_selections, original.tool_selections);
    assert_eq!(loaded.inference_backends, original.inference_backends);
    assert_eq!(loaded.inference_profiles, original.inference_profiles);
    assert_eq!(
        loaded.tool_service_registries,
        original.tool_service_registries
    );
    assert_eq!(loaded.tasks, original.tasks);
    assert_eq!(loaded.schedules, original.schedules);
}

mod load_manifest_root {
    use crate::desired_state::load::load_manifest_root;
    use std::fs;
    use tempfile::tempdir;

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

    // Ported from PR #68 (EventTrigger): adapted from flat-file format to the
    // per-doc layout introduced by PR #67.
    #[test]
    fn loads_event_trigger_from_per_doc_dir() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        // Add a task so the event trigger's task_id reference is valid.
        let task_dir = tmp.path().join("tasks").join("summarize-inbox");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(
            task_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "task_id": "summarize-inbox",
                "name": "Summarize inbox",
                "behavior_id": "default",
                "prompt_template": "Summarize the unread emails.",
                "enabled": true,
            }))
            .unwrap(),
        )
        .unwrap();

        // Add an event trigger in the per-doc directory layout.
        let trigger_dir = tmp.path().join("event_triggers").join("new-customer-greet");
        fs::create_dir_all(&trigger_dir).unwrap();
        fs::write(
            trigger_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "trigger_id": "new-customer-greet",
                "task_id": "summarize-inbox",
                "source_collection": "CustomerSignup",
                "event_kind": "created",
                "enabled": true,
                "concurrency": "serial",
            }))
            .unwrap(),
        )
        .unwrap();

        let (manifest, report) = load_manifest_root(tmp.path());
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

    // Ported from PR #68: deprecated capability fields on DesiredInferenceBackend
    // must be ignored by serde (deny_unknown_fields is NOT set on that struct).
    #[test]
    fn deprecated_backend_capability_fields_are_ignored() {
        let tmp = tempdir().unwrap();
        write_minimal_root(tmp.path());

        // Write an inference-backend per-doc dir with deprecated capability fields.
        let backend_dir = tmp.path().join("inference-backends").join("local");
        fs::create_dir_all(&backend_dir).unwrap();
        // Write with deprecated capability fields; serde must ignore them.
        fs::write(
            backend_dir.join("object.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
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
                "models": ["test-model"],
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, report) = load_manifest_root(tmp.path());
        assert!(
            report.ok,
            "expected valid manifest, got {:?}",
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
fn diff_manifests_marks_tool_selection_update_when_mcp_allowlist_changes() {
    let mut desired = empty_manifest("did:defra-agent:test");
    let mut desired_selection = sample_tool_selection("service-tools");
    desired_selection.allowed_mcp_service_ids = vec!["x-data".to_string()];
    desired.tool_selections.push(desired_selection);

    let mut live = empty_manifest("did:defra-agent:test");
    live.tool_selections
        .push(sample_tool_selection("service-tools"));

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &desired,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(
        report.collections.tool_selections.update,
        vec!["service-tools"]
    );
    assert!(report.collections.tool_selections.create.is_empty());
    assert!(report.collections.tool_selections.unchanged.is_empty());
    assert_eq!(report.counts.tool_selections.update, 1);
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

#[test]
fn diff_manifests_creates_event_trigger_when_live_is_empty() {
    let manifest = empty_manifest_with_event_trigger(sample_event_trigger());
    let live = empty_manifest("did:defra-agent:test");

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &manifest,
        Some(&live.agent_principal),
        &live,
    );

    assert_eq!(
        report.collections.event_triggers.create,
        vec!["new-customer-greet"]
    );
    assert!(report.collections.event_triggers.update.is_empty());
    assert!(report.collections.event_triggers.unchanged.is_empty());
    assert!(report.collections.event_triggers.live_only.is_empty());
    assert_eq!(report.counts.event_triggers.create, 1);
}

#[test]
fn diff_manifests_marks_event_trigger_update_when_filter_changes() {
    let mut desired = sample_event_trigger();
    desired.filter = Some(r#"{ plan: { _eq: "paid" } }"#.to_string());
    let live = sample_event_trigger();
    let manifest = empty_manifest_with_event_trigger(desired);
    let live_manifest = empty_manifest_with_event_trigger(live);

    let report = diff_manifests(
        &PathBuf::from("/tmp/fake-root"),
        "local",
        &manifest,
        Some(&live_manifest.agent_principal),
        &live_manifest,
    );

    assert_eq!(
        report.collections.event_triggers.update,
        vec!["new-customer-greet"]
    );
    assert!(report.collections.event_triggers.create.is_empty());
    assert!(report.collections.event_triggers.unchanged.is_empty());
}

fn validation_errors(manifest: &DesiredStateManifest) -> Vec<String> {
    let mut errors = Vec::new();
    validate_manifest(manifest, &mut errors);
    errors
}

// ── subagent_targets structural validation ──────────────────────────────────

#[test]
fn validate_rejects_empty_string_in_subagent_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:defra-agent:test".to_string();
    sel.subagent_targets = Some(vec!["".to_string()]);
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|msg| msg.contains("subagent_targets") && msg.contains("agent-tools")),
        "expected empty subagent_targets entry rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_subagent_spawn_enabled_without_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:defra-agent:test".to_string();
    sel.subagent_spawn_enabled = Some(true);
    sel.subagent_targets = None;
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("agent-tools")
                && msg.contains("subagent_spawn_enabled")
                && msg.contains("subagent_targets")
        }),
        "expected subagent_spawn_enabled-without-targets rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_subagent_spawn_enabled_with_empty_targets_vec() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:defra-agent:test".to_string();
    sel.subagent_spawn_enabled = Some(true);
    sel.subagent_targets = Some(vec![]);
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("agent-tools")
                && msg.contains("subagent_spawn_enabled")
                && msg.contains("subagent_targets")
        }),
        "expected subagent_spawn_enabled-with-empty-targets-vec rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_subagent_spawn_enabled_with_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:defra-agent:test".to_string();
    sel.subagent_spawn_enabled = Some(true);
    sel.subagent_targets = Some(vec!["amy-research".to_string()]);
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("subagent_targets") || msg.contains("subagent_spawn_enabled")),
        "expected no subagent rejections for valid config, got {errors:?}"
    );
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

fn sample_event_trigger_for(trigger_id: &str, task_id: &str) -> DesiredEventTrigger {
    DesiredEventTrigger {
        trigger_id: trigger_id.to_string(),
        task_id: task_id.to_string(),
        source_collection: "CustomerSignup".to_string(),
        event_kind: "created".to_string(),
        filter: None,
        enabled: true,
        concurrency: "serial".to_string(),
    }
}

#[test]
fn validate_rejects_event_trigger_referencing_unknown_task() {
    let mut manifest = manifest_with_default_behavior();
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "missing-task",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet")
                && message.contains("unknown task_id")
                && message.contains("missing-task")
        }),
        "expected unknown task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_unknown_event_kind() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut trig = sample_event_trigger_for("new-customer-greet", "summarize-inbox");
    trig.event_kind = "updated".to_string();
    manifest.event_triggers.push(trig);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet") && message.contains("unsupported event_kind")
        }),
        "expected unsupported event_kind rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_template_referencing_args_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ args.foo }}".to_string();
    manifest.tasks.push(task);
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet") && message.contains("forbidden scope: args")
        }),
        "expected event-trigger forbidden-args rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_event_trigger_template_using_event_and_doc_scopes() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ event.fired_at }} {{ doc.name }}".to_string();
    manifest.tasks.push(task);
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("forbidden scope")),
        "expected no forbidden-scope rejections for event+doc scopes, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_event_trigger_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate") && message.contains("new-customer-greet")
        }),
        "expected duplicate trigger_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_unknown_concurrency() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut trig = sample_event_trigger_for("new-customer-greet", "summarize-inbox");
    trig.concurrency = "weird".to_string();
    manifest.event_triggers.push(trig);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet")
                && message.contains("unknown concurrency")
                && message.contains("weird")
        }),
        "expected unknown concurrency rejection, got {errors:?}"
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
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("prompt.md"), "You are a helpful agent.").unwrap();

    let mut value = Some("./prompt.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_leaves_literal_string_untouched() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let mut value = Some("You are a helpful agent.".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("You are a helpful agent."));
}

#[test]
fn hydrate_sidecar_ignores_absolute_path() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let mut value = Some("/etc/hosts".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("/etc/hosts"));
}

#[test]
fn hydrate_sidecar_ignores_parent_relative_path() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let mut value = Some("../elsewhere.md".to_string());
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert_eq!(value.as_deref(), Some("../elsewhere.md"));
}

#[test]
fn hydrate_sidecar_rejects_parent_component_in_rel_path() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    // Create a sibling file OUTSIDE the doc directory.
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("doc-dir");
    fs::create_dir_all(&json_dir).unwrap();
    fs::write(tmp.path().join("sibling.md"), "secret contents").unwrap();

    // ./../sibling.md has a ParentDir component in the relative part.
    let mut value = Some("./../sibling.md".to_string());
    let err = hydrate_sidecar(&mut value, &json_dir).unwrap_err();
    assert!(err.contains("escapes document directory"), "got: {err}");
}

#[test]
fn hydrate_sidecar_rejects_nested_parent_component() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("a").join("b");
    fs::create_dir_all(&json_dir).unwrap();
    fs::write(tmp.path().join("outside.md"), "not yours").unwrap();

    // ./inner/../../outside.md resolves outside the doc dir.
    let mut value = Some("./inner/../../outside.md".to_string());
    let err = hydrate_sidecar(&mut value, &json_dir).unwrap_err();
    assert!(err.contains("escapes document directory"), "got: {err}");
}

#[test]
fn hydrate_sidecar_errors_when_file_missing() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let mut value = Some("./missing.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("sidecar path does not resolve"), "got: {err}");
    assert!(err.contains("missing.md"), "got: {err}");
}

#[test]
fn hydrate_sidecar_errors_on_non_utf8() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("bad.md"), [0xff, 0xfe, 0xfd]).unwrap();
    let mut value = Some("./bad.md".to_string());
    let err = hydrate_sidecar(&mut value, dir.path()).unwrap_err();
    assert!(err.contains("not valid UTF-8"), "got: {err}");
}

#[test]
fn hydrate_sidecar_is_noop_on_none() {
    use super::load::hydrate_sidecar;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let mut value: Option<String> = None;
    hydrate_sidecar(&mut value, dir.path()).unwrap();
    assert!(value.is_none());
}

mod load_per_doc_collection {
    use crate::desired_state::load::load_per_doc_collection;
    use crate::desired_state::{DesiredAgentBehavior, HasUniqueId};
    use defra_agent::Collection;
    use std::fs;
    use tempfile::tempdir;

    fn write_behavior_dir(root: &std::path::Path, handle: &str, behavior_id: &str) {
        let dir = root.join("agent-behaviors").join(handle);
        fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "behavior_id": behavior_id,
            "agent_did": "did:key:example",
            "enabled": true,
        });
        fs::write(
            dir.join("object.json"),
            serde_json::to_vec_pretty(&body).unwrap(),
        )
        .unwrap();
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
        assert!(
            errors[0].contains("is missing object.json"),
            "got: {:?}",
            errors
        );
    }

    #[test]
    fn handle_mismatch_is_error() {
        let tmp = tempdir().unwrap();
        write_behavior_dir(tmp.path(), "on-disk-name", "id-inside-json");
        let mut errors = Vec::new();
        let _: Vec<DesiredAgentBehavior> =
            load_per_doc_collection(tmp.path(), Collection::AgentBehavior, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("does not match behavior_id"),
            "got: {:?}",
            errors
        );
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
            tmp.path()
                .join("agent-behaviors")
                .join("default")
                .join("README.md"),
            "notes",
        )
        .unwrap();
        fs::write(
            tmp.path()
                .join("agent-behaviors")
                .join("default")
                .join(".DS_Store"),
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
        write_manifest_root, DesiredAgentBehavior, DesiredAgentPrincipal, DesiredStateManifest,
        DesiredTask,
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
            event_triggers: Vec::new(),
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
        assert_eq!(
            fs::read_to_string(&behavior_sidecar).unwrap(),
            "You are helpful."
        );
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
        assert_eq!(
            fs::read_to_string(&task_sidecar).unwrap(),
            "Check the fleet."
        );
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

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("leftover.txt"), "junk").unwrap();
        let err = write_manifest_root(tmp.path(), &minimal_manifest(), false).unwrap_err();
        assert!(err.contains("--force"), "got: {err}");
    }

    #[test]
    fn preflight_unsafe_id_does_not_delete_existing_root() {
        // Regression: if force=true and the manifest contains an unsafe id,
        // the old root must be left intact (pre-flight runs before prepare_root).
        let tmp = tempdir().unwrap();

        // Set up a pre-existing valid manifest root.
        fs::write(
            tmp.path().join("agent-principal.json"),
            b"{\"agent_did\":\"did:key:old\",\"enabled\":true}",
        )
        .unwrap();
        let old_behavior_dir = tmp.path().join("agent-behaviors").join("old-safe-id");
        fs::create_dir_all(&old_behavior_dir).unwrap();
        fs::write(old_behavior_dir.join("object.json"), b"{}").unwrap();

        // Build a manifest where the second behavior has an unsafe id.
        let mut bad_manifest = minimal_manifest();
        // First behavior (inherited from minimal_manifest) is fine.
        // Add a second behavior with a path-traversal id.
        bad_manifest.agent_behaviors.push(DesiredAgentBehavior {
            behavior_id: "bad/id".to_string(),
            agent_did: "did:key:example".to_string(),
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

        let err = write_manifest_root(tmp.path(), &bad_manifest, true).unwrap_err();
        assert!(err.contains("filesystem-unsafe"), "got: {err}");

        // The old root must still be intact: pre-flight must have run before
        // prepare_root deleted anything.
        assert!(
            tmp.path().join("agent-principal.json").exists(),
            "old agent-principal.json was deleted before pre-flight finished"
        );
        assert!(
            old_behavior_dir.join("object.json").exists(),
            "old behavior dir was deleted before pre-flight finished"
        );
    }

    #[test]
    fn force_removes_stray_files_from_previous_export() {
        let tmp = tempdir().unwrap();
        // Simulate a previous export: agent-principal.json present (sentinel) plus a
        // stale behavior directory from a prior run.
        fs::write(
            tmp.path().join("agent-principal.json"),
            b"{\"agent_did\":\"did:key:stale\",\"enabled\":false}",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("agent-behaviors").join("old-name")).unwrap();
        fs::write(
            tmp.path().join("agent-behaviors/old-name/object.json"),
            b"{}",
        )
        .unwrap();
        fs::write(tmp.path().join("leftover.txt"), "junk").unwrap();

        write_manifest_root(tmp.path(), &minimal_manifest(), true).unwrap();

        // Leftover is gone.
        assert!(!tmp.path().join("leftover.txt").exists());
        // Old behavior dir is gone.
        assert!(!tmp.path().join("agent-behaviors/old-name").exists());
        // New content is present.
        assert!(tmp
            .path()
            .join("agent-behaviors/default/object.json")
            .is_file());
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
    fn rejects_null_byte() {
        assert!(
            check_filesystem_safe_id("a\0b").is_err(),
            "should reject null byte"
        );
    }

    #[test]
    fn accepts_colon_in_human_keys() {
        assert!(
            check_filesystem_safe_id("profile:default").is_ok(),
            "colons are legal on POSIX"
        );
        assert!(check_filesystem_safe_id("tools:default").is_ok());
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

    #[test]
    fn rejects_dot_prefix() {
        let err = check_filesystem_safe_id(".foo").unwrap_err();
        assert!(err.contains("dot-prefixed"), "got: {err}");
        let err = check_filesystem_safe_id(".hidden").unwrap_err();
        assert!(err.contains("dot-prefixed"), "got: {err}");
    }
}
