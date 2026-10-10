use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::tool_surface::{
    AgentToolConfig, AgentToolSurfaceConfig, FileToolMode, ResolvedToolSelection,
    RuntimeToolAvailability, ToolCeiling,
};

fn fingerprint_tool_surface(lsp_config: Option<String>) -> Arc<ToolSurface> {
    let enable_lsp = lsp_config.is_some();
    let file_tools = if enable_lsp {
        FileToolMode::ReadWrite
    } else {
        FileToolMode::Off
    };
    Arc::new(
        AgentToolSurfaceConfig::from_selection(
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
        .resolve_with_agent_tools_for_runtime_availability(
            RuntimeToolAvailability::all(),
            AgentToolConfig::default(),
        ),
    )
}

#[test]
fn readiness_source_validation_rejects_noncanonical_or_unassigned_defaults() {
    let mut resolved = ResolvedRuntimeSnapshot {
        node: None,
        local_did: String::new(),
        default_agent_id: "missing".to_string(),
        agents: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_agents: HashMap::from([(
            "general".to_string(),
            UnavailableAgent::new(
                AgentReadinessUnavailableReason::BackendNotConfigured,
                "missing backend",
            ),
        )]),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    };
    assert!(resolved.validate_node_readiness_source().is_err());

    resolved.default_agent_id = " general".to_string();
    assert!(resolved.validate_node_readiness_source().is_err());

    resolved.default_agent_id = "general".to_string();
    assert!(resolved.validate_node_readiness_source().is_ok());
}

#[test]
fn configuration_fingerprint_reflects_schedule_set() {
    let base = ResolvedRuntimeSnapshot {
        node: None,
        local_did: String::new(),
        default_agent_id: "general".to_string(),
        agents: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_agents: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
    };
    let baseline = base.configuration_fingerprint();

    let task = ResolvedTask {
        emit_outcome: false,
        task_id: "t1".to_string(),
        name: None,
        agent_id: "general".to_string(),
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
                session_id_template: None,
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
        node: None,
        local_did: String::new(),
        default_agent_id: "general".to_string(),
        agents: HashMap::new(),
        tool_surfaces: HashMap::from([(
            "general".to_string(),
            fingerprint_tool_surface(Some("{}".to_string())),
        )]),
        backend_admission_configs: HashMap::new(),
        unavailable_agents: HashMap::new(),
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
