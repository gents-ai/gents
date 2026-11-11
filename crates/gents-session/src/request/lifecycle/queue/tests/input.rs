use super::*;

#[test]
fn request_origin_names_what_put_a_request_into_its_session() {
    use crate::request::lifecycle::{request_origin, RequestOrigin};
    use gents_protocol::row::AgentRequestRow;

    let row = |value: serde_json::Value| -> AgentRequestRow {
        serde_json::from_value(value).expect("request row")
    };
    assert_eq!(
        request_origin(&row(serde_json::json!({ "request_id": "typed" }))),
        RequestOrigin::Person
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "answer",
            "caused_by_source_doc_id": "mailbox-item"
        }))),
        RequestOrigin::Person
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "steered",
            "input": { "queue": { "source": "steering", "policy": "append" } }
        }))),
        RequestOrigin::Person
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "sent",
            "caused_by_parent_request_doc_id": "parent-doc",
            "caused_by_parent_tool_call_doc_id": "call-doc",
            "input": { "queue": { "source": "steering", "policy": "append" } }
        }))),
        RequestOrigin::SessionMessage {
            parent_request_doc_id: Some("parent-doc")
        }
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "fired",
            "caused_by_trigger_id": "nightly",
            "caused_by_trigger_kind": "schedule"
        }))),
        RequestOrigin::Trigger {
            trigger_id: "nightly",
            trigger_kind: Some("schedule")
        }
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "continued",
            "caused_by_trigger_id": "goal-1",
            "caused_by_trigger_kind": "goal",
            "input": {
                "queue": { "source": "goal", "policy": "coalesce", "key": "goal:x" },
                "goal_continuation": { "sequence": 2, "wrapup": false }
            }
        }))),
        RequestOrigin::GoalContinuation {
            goal_id: Some("goal-1"),
            sequence: Some(2),
            wrapup: Some(false),
        }
    );
    assert_eq!(
        request_origin(&row(serde_json::json!({
            "request_id": "woke",
            "input": {
                "queue": {
                    "source": "background_completion",
                    "policy": "coalesce",
                    "key": "background_completion:s",
                    "background_completion_wake_version": 1
                }
            }
        }))),
        RequestOrigin::BackgroundCompletion
    );
}
