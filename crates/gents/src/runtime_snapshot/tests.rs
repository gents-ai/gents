use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::tool_surface::{
    BehaviorToolConfig, FileToolMode, ResolvedToolSelection, RuntimeToolAvailability,
    SubagentToolConfig, ToolCeiling,
};

fn fingerprint_tool_surface(lsp_config: Option<String>) -> Arc<ToolSurface> {
    let enable_lsp = lsp_config.is_some();
    let file_tools = if enable_lsp {
        FileToolMode::ReadWrite
    } else {
        FileToolMode::Off
    };
    Arc::new(
        BehaviorToolConfig::from_selection(
            "fingerprint",
            ResolvedToolSelection {
                file_tools,
                enable_lsp,
                lsp_config,
                ..ResolvedToolSelection::default()
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
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let with_schedule = base.clone().with_automation(ResolvedAutomation {
        schedules: HashMap::from([(
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
        ..Default::default()
    });
    assert_ne!(baseline, with_schedule.configuration_fingerprint());

    let with_unavailable = base.clone().with_automation(ResolvedAutomation {
        unavailable_schedules: HashSet::from(["s2".to_string()]),
        ..Default::default()
    });
    assert_ne!(baseline, with_unavailable.configuration_fingerprint());
}

#[test]
fn configuration_fingerprint_reflects_lsp_configuration() {
    let base = ResolvedRuntimeSnapshot {
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::from([(
            "general".to_string(),
            fingerprint_tool_surface(Some("{}".to_string())),
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

    let mut lsp_changed = base;
    lsp_changed.tool_surfaces.insert(
        "general".to_string(),
        fingerprint_tool_surface(Some(r#"{"format_on_write":true}"#.to_string())),
    );
    assert_ne!(baseline, lsp_changed.configuration_fingerprint());
}
