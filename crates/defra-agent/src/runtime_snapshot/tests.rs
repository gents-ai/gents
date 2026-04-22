use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::*;

fn snapshot(generation: u64, default_behavior_id: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation,
        default_behavior_id: default_behavior_id.to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        dispatchers: HashMap::new(),
    })
}

#[test]
fn resolved_snapshot_activate_preserves_generation_and_dispatchers() {
    let resolved = ResolvedRuntimeSnapshot {
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::from([("code".to_string(), "missing backend".to_string())]),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
    };
    let (general_tx, _general_rx) = mpsc::channel(1);
    let active = resolved.activate(1, HashMap::from([("general".to_string(), general_tx)]));

    assert_eq!(active.generation, 1);
    assert_eq!(active.default_behavior_id, "general");
    assert!(active.dispatchers.contains_key("general"));
    assert_eq!(active.unavailable_reason("code"), Some("missing backend"));
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
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
    };
    let baseline = base.configuration_fingerprint();

    let task = ResolvedTask {
        task_id: "t1".to_string(),
        behavior_id: "general".to_string(),
        prompt_template: "do the thing".to_string(),
        output_schema_ref: None,
    };
    let with_schedule = base.clone().with_schedules(
        HashMap::from([(
            "s1".to_string(),
            ResolvedSchedule {
                schedule_id: "s1".to_string(),
                task_id: "t1".to_string(),
                task: task.clone(),
                interval_secs: 60,
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
