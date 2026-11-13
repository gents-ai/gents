use crate::{Trigger, BUDGET_EXHAUSTED_STATUS, DEFAULT_MAX_FIRES_PER_HOUR};

fn trigger_with_budget(budget: Option<u32>, receipted: bool) -> Trigger {
    let source = if receipted {
        serde_json::json!({"kind": "event", "event_source_id": "s"})
    } else {
        serde_json::json!({"kind": "schedule", "schedule_id": "s"})
    };
    serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zOwner", "trigger_id": "t", "task_id": "task",
        "source": source,
        "max_fires_per_hour": budget,
    }))
    .unwrap()
}

#[test]
fn fire_budget_config_matches_the_lean_model() {
    gents_lean_vocab::families::trigger_delivery_fire_budget_config::replay(|input| {
        trigger_with_budget(
            input.configured.map(|value| value as u32),
            input.receipted,
        )
        .validate_options()
        .is_ok()
    });
}

#[test]
fn fire_budget_default_matches_the_lean_model() {
    gents_lean_vocab::families::trigger_delivery_fire_budget_default::replay(|_| {
        u64::from(DEFAULT_MAX_FIRES_PER_HOUR)
    });
}

#[test]
fn fire_budget_status_matches_the_lean_model() {
    gents_lean_vocab::families::trigger_delivery_fire_budget_status::replay(|_| {
        BUDGET_EXHAUSTED_STATUS.to_owned()
    });
}
