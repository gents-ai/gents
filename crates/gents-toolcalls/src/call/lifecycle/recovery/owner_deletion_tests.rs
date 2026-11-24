use defra_node::QueryResponse;
use gents_store::config_client::ConfigClientError;
use serde_json::json;

use super::{task_deleted, trigger_state, TriggerState};

fn answer(data: serde_json::Value) -> Result<QueryResponse, ConfigClientError> {
    Ok(serde_json::from_value(json!({ "data": data })).expect("query response"))
}

#[test]
fn a_named_trigger_or_task_the_node_can_no_longer_read_is_deleted() {
    assert_eq!(
        trigger_state(answer(json!({ "Trigger": [] })), "trigger").unwrap(),
        TriggerState::Deleted
    );
    assert!(task_deleted(answer(json!({ "Task": [] })), "task").unwrap());
}

#[test]
fn a_trigger_or_task_read_back_deleted_is_deleted() {
    assert_eq!(
        trigger_state(
            answer(json!({ "Trigger": [{ "_deleted": true, "task_id": "task" }] })),
            "trigger"
        )
        .unwrap(),
        TriggerState::Deleted
    );
    assert!(task_deleted(answer(json!({ "Task": [{ "_deleted": true }] })), "task").unwrap());
}

#[test]
fn a_live_readable_trigger_and_task_are_not_deleted() {
    assert_eq!(
        trigger_state(
            answer(json!({ "Trigger": [{ "_deleted": false, "task_id": "task" }] })),
            "trigger"
        )
        .unwrap(),
        TriggerState::Live {
            task_id: Some("task".into())
        }
    );
    assert!(!task_deleted(answer(json!({ "Task": [{ "_deleted": false }] })), "task").unwrap());
    assert!(!task_deleted(
        answer(json!({ "Task": [{ "_deleted": true }, { "_deleted": false }] })),
        "task"
    )
    .unwrap());
}

#[test]
fn an_access_check_without_an_answer_or_that_failed_decides_nothing() {
    for error in [
        || ConfigClientError::DecisionReadUnanswered,
        || ConfigClientError::DecisionReadFailed,
    ] {
        assert!(trigger_state(Err(error()), "trigger").is_err());
        assert!(task_deleted(Err(error()), "task").is_err());
    }
}

#[test]
fn an_answer_without_the_collection_decides_nothing() {
    assert!(trigger_state(answer(json!({})), "trigger").is_err());
    assert!(task_deleted(answer(json!({ "Task": null })), "task").is_err());
    assert!(task_deleted(answer(serde_json::Value::Null), "task").is_err());
}
