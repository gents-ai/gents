use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::*;

fn snapshot(generation: u64, default_behavior_id: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation,
        default_behavior_id: default_behavior_id.to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        dispatchers: HashMap::new(),
    })
}

#[test]
fn resolved_snapshot_activate_preserves_generation_and_dispatchers() {
    let resolved = ResolvedRuntimeSnapshot {
        default_behavior_id: "general".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        unavailable_behaviors: HashMap::from([("code".to_string(), "missing backend".to_string())]),
    };
    let (general_tx, _general_rx) = mpsc::channel(1);
    let active = resolved.activate(1, HashMap::from([("general".to_string(), general_tx)]));

    assert_eq!(active.generation, 1);
    assert_eq!(active.default_behavior_id, "general");
    assert!(active.dispatchers.contains_key("general"));
    assert_eq!(active.unavailable_reason("code"), Some("missing backend"));
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
