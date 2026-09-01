use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::*;
use crate::identity::{AgentIdentity as _, AgentPrincipal, KeyIdentity};
use crate::tool_surface::{
    BehaviorToolConfig, FileToolMode, RuntimeToolAvailability, SubagentToolConfig, ToolCeiling,
    ToolSelection,
};

/// Build a minimal `Arc<AgentPrincipal>` for tests that call `.activate()`.
/// Does not exercise signing — only satisfies the principal invariant so that
/// the `debug_assert!` in `activate()` does not fire.
fn stub_principal() -> Arc<AgentPrincipal> {
    let identity = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("stub-principal-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    Arc::new(AgentPrincipal {
        agent_did: identity.did().to_string(),
        identity,
        default_behavior_id: String::new(),
        display_name: None,
        enabled: true,
    })
}

fn snapshot(generation: u64, default_behavior_id: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation,
        principal: None,
        local_did: String::new(),
        default_behavior_id: default_behavior_id.to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    })
}

fn fingerprint_tool_surface(
    approval_required_tools: Vec<String>,
    lsp_config: Option<String>,
) -> Arc<ToolSurface> {
    let enable_lsp = lsp_config.is_some();
    let file_tools = if enable_lsp {
        FileToolMode::ReadWrite
    } else {
        FileToolMode::Off
    };
    Arc::new(
        BehaviorToolConfig::from_selection(
            "fingerprint",
            ToolSelection {
                file_tools,
                approval_required_tools,
                enable_lsp,
                lsp_config,
                ..ToolSelection::default()
            },
            &ToolCeiling::readwrite(std::env::temp_dir()),
            Vec::new(),
        )
        .unwrap()
        .resolve_with_subagent_tools_for_runtime_availability(
            RuntimeToolAvailability::all(),
            SubagentToolConfig::default(),
        ),
    )
}

#[test]
fn resolved_snapshot_activate_preserves_generation_and_dispatchers() {
    let resolved = ResolvedRuntimeSnapshot {
        principal: None,
        local_did: "did:local".to_string(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::from([(
            "code".to_string(),
            UnavailableBehavior::new(
                BehaviorReadinessUnavailableReason::BackendNotConfigured,
                "missing backend",
            ),
        )]),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    }
    .with_principal(stub_principal());
    let (general_tx, _general_rx) = mpsc::channel(1);
    let active = resolved.activate(1, HashMap::from([("general".to_string(), general_tx)]));

    assert_eq!(active.generation, 1);
    assert_eq!(active.default_behavior_id, "general");
    assert_eq!(active.local_did, "did:local");
    assert!(active.dispatchers.contains_key("general"));
    assert_eq!(
        active.unavailable_diagnostic("code"),
        Some("missing backend")
    );
}

#[test]
fn readiness_source_validation_rejects_noncanonical_or_unassigned_defaults() {
    let mut resolved = ResolvedRuntimeSnapshot {
        principal: None,
        local_did: String::new(),
        default_behavior_id: "missing".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::from([(
            "general".to_string(),
            UnavailableBehavior::new(
                BehaviorReadinessUnavailableReason::BackendNotConfigured,
                "missing backend",
            ),
        )]),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    };
    assert!(resolved.validate_behavior_readiness_source().is_err());

    resolved.default_behavior_id = " general".to_string();
    assert!(resolved.validate_behavior_readiness_source().is_err());

    resolved.default_behavior_id = "general".to_string();
    assert!(resolved.validate_behavior_readiness_source().is_ok());
}

#[test]
fn concurrency_mode_parse_accepts_exact_known_values() {
    assert_eq!(
        ConcurrencyMode::parse("parallel"),
        Some(ConcurrencyMode::Parallel)
    );
    assert_eq!(
        ConcurrencyMode::parse("serial"),
        Some(ConcurrencyMode::Serial)
    );
    assert_eq!(
        ConcurrencyMode::parse("latest_only"),
        Some(ConcurrencyMode::LatestOnly)
    );
}

#[test]
fn concurrency_mode_parse_is_strict() {
    assert_eq!(ConcurrencyMode::parse("Parallel"), None);
    assert_eq!(ConcurrencyMode::parse("SERIAL"), None);
    assert_eq!(ConcurrencyMode::parse("latest-only"), None);
    assert_eq!(ConcurrencyMode::parse("latestOnly"), None);
    assert_eq!(ConcurrencyMode::parse(" parallel "), None);
    assert_eq!(ConcurrencyMode::parse(""), None);
}

#[test]
fn configuration_fingerprint_reflects_schedule_set() {
    let base = ResolvedRuntimeSnapshot {
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    };
    let baseline = base.configuration_fingerprint();

    let task = ResolvedTask {
        task_id: "t1".to_string(),
        name: None,
        behavior_id: "general".to_string(),
        prompt_template: "do the thing".to_string(),
        output_schema_ref: None,
    };
    let with_schedule = base.clone().with_schedules(
        HashMap::from([(
            "s1".to_string(),
            ResolvedSchedule {
                trigger_doc_id: "s1-doc".to_string(),
                schedule_id: "s1".to_string(),
                task_id: "t1".to_string(),
                task: task.clone(),
                cadence: ScheduleCadence::Interval { interval_secs: 60 },
                enabled: true,
                concurrency: ConcurrencyMode::Serial,
            },
        )]),
        HashSet::new(),
    );
    assert_ne!(baseline, with_schedule.configuration_fingerprint());

    let with_unavailable = base
        .clone()
        .with_schedules(HashMap::new(), HashSet::from(["s2".to_string()]));
    assert_ne!(baseline, with_unavailable.configuration_fingerprint());
}

#[test]
fn configuration_fingerprint_reflects_approval_and_lsp_configuration() {
    let base = ResolvedRuntimeSnapshot {
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::from([(
            "general".to_string(),
            fingerprint_tool_surface(Vec::new(), Some("{}".to_string())),
        )]),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    };
    let baseline = base.configuration_fingerprint();

    let mut held = base.clone();
    held.tool_surfaces.insert(
        "general".to_string(),
        fingerprint_tool_surface(vec!["bash".to_string()], Some("{}".to_string())),
    );
    assert_ne!(baseline, held.configuration_fingerprint());

    let mut lsp_changed = base;
    lsp_changed.tool_surfaces.insert(
        "general".to_string(),
        fingerprint_tool_surface(Vec::new(), Some(r#"{"format_on_write":true}"#.to_string())),
    );
    assert_ne!(baseline, lsp_changed.configuration_fingerprint());
}

#[test]
fn refresh_active_snapshot_updates_to_new_generation() {
    let initial = snapshot(1, "general");
    let updated = snapshot(2, "code");
    let (tx, mut rx) = watch::channel(initial.clone());
    let mut current = initial;

    tx.send(updated.clone()).unwrap();

    assert!(refresh_active_snapshot(&mut current, &mut rx));
    assert!(Arc::ptr_eq(&current, &updated));
    assert_eq!(current.generation, 2);
    assert_eq!(current.default_behavior_id, "code");
}

#[test]
fn refresh_active_snapshot_is_noop_when_unchanged() {
    let initial = snapshot(1, "general");
    let (_tx, mut rx) = watch::channel(initial.clone());
    let mut current = initial.clone();

    assert!(!refresh_active_snapshot(&mut current, &mut rx));
    assert!(Arc::ptr_eq(&current, &initial));
}
