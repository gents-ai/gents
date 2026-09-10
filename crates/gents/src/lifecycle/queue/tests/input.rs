use super::*;

fn queue_input(queue: RequestQueue) -> RequestInput {
    RequestInput {
        queue: Some(queue),
        ..Default::default()
    }
}

#[test]
fn rejects_noncanonical_queue_source_on_decode() {
    // The canonical RequestQueue enum has no `subagent_completion` source;
    // a row decoded with it must fail rather than pass through.
    let decoded: Result<gents_protocol::row::AgentRequestRow, _> =
        serde_json::from_value(serde_json::json!({
            "request_id": "req-1",
            "input": {
                "queue": {
                    "source": "subagent_completion",
                    "policy": "coalesce",
                    "key": "session:sess-1",
                    "queued_after_request_id": "req-1"
                }
            }
        }));
    assert!(decoded.is_err(), "unknown queue source must fail decoding");
}

#[test]
fn queue_source_and_policy_vocabulary_round_trips_canonically() {
    for (source_name, source) in [
        ("user", QueueSource::User),
        ("background_completion", QueueSource::BackgroundCompletion),
        ("steering", QueueSource::Steering),
        ("goal", QueueSource::Goal),
    ] {
        for (policy_name, policy) in [
            ("append", QueuePolicy::Append),
            ("coalesce", QueuePolicy::Coalesce),
        ] {
            let queue = RequestQueue {
                source,
                policy,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            };
            let encoded = serde_json::to_value(queue_input(queue.clone())).unwrap();
            assert_eq!(encoded["queue"]["source"], source_name);
            assert_eq!(encoded["queue"]["policy"], policy_name);
            assert_eq!(
                serde_json::from_value::<RequestInput>(encoded)
                    .unwrap()
                    .queue,
                Some(queue)
            );
        }
    }
}

#[test]
fn rejects_invalid_queue_json_and_unknown_keys() {
    assert!(serde_json::from_str::<RequestInput>("not json").is_err());
    assert!(serde_json::from_str::<RequestInput>(r#"{"run_id":"abc"}"#).is_err());
    assert!(serde_json::from_str::<RequestInput>(
        r#"{"queue":{"source":"timer","policy":"append"}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<RequestInput>(
        r#"{"queue":{"source":"user","policy":"append","wake":1}}"#
    )
    .is_err());
}
#[test]
fn automated_wakeup_is_true_only_for_keyed_background_completion_coalesce() {
    assert!(!is_automated_wakeup(&RequestInput::default()));
    assert!(!is_automated_wakeup(&queue_input(RequestQueue {
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    })));
    assert!(is_automated_wakeup(&queue_input(RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some("background_completion:session-1".to_string()),
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    })));
    assert!(!is_automated_wakeup(&queue_input(RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Append,
        key: Some("background_completion:session-1".to_string()),
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    })));
    assert!(!is_automated_wakeup(&queue_input(RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    })));
    assert!(!is_automated_wakeup(&queue_input(RequestQueue {
        source: QueueSource::Steering,
        policy: QueuePolicy::Coalesce,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    })));
}

#[test]
fn wake_version_is_stamped_by_the_queue_owner() {
    let queue = background_wake_queue(
        &RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:session-1".to_string()),
            queued_after_request_id: Some("parent".to_string()),
            interrupted_request_id: Some("interrupted".to_string()),
            background_completion_wake_version: None,
        },
        Some("parent".to_string()),
    );
    assert_eq!(
        queue,
        RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:session-1".to_string()),
            queued_after_request_id: Some("parent".to_string()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(BACKGROUND_COMPLETION_WAKE_VERSION),
        }
    );
}

#[test]
fn runtime_control_projection_keeps_only_the_steering_input_visible() {
    let input = |source| {
        queue_input(RequestQueue {
            source,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: None,
        })
    };
    let steering = input(QueueSource::Steering);
    let steering_input = steering_input_message_key("request-1");

    assert!(!crate::lifecycle::is_runtime_control_message(
        &steering,
        &steering_input,
        true,
    ));
    assert!(crate::lifecycle::is_runtime_control_message(
        &steering, "", true,
    ));
    assert!(crate::lifecycle::is_runtime_control_message(
        &input(QueueSource::Goal),
        "",
        false,
    ));
    assert!(crate::lifecycle::is_runtime_control_message(
        &RequestInput::default(),
        "background-completion-notification:child-1:subagent",
        false,
    ));
    assert!(!crate::lifecycle::is_runtime_control_message(
        &input(QueueSource::User),
        "",
        false,
    ));
    assert!(!crate::lifecycle::is_runtime_control_message(
        &steering,
        "session-1:4",
        false,
    ));
    assert!(!crate::lifecycle::request_content_owns_user_projection(
        &steering
    ));
    assert!(crate::lifecycle::request_content_owns_user_projection(
        &RequestInput::default()
    ));
    assert!(crate::lifecycle::request_owns_user_turn(&steering));
    assert!(!crate::lifecycle::request_owns_user_turn(&input(
        QueueSource::Goal
    )));
}
