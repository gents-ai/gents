use super::*;

#[test]
fn parses_queue_hints_from_metadata_queue_field() {
    let metadata = r#"{
        "queue": {
            "source": "subagent_completion",
            "policy": "coalesce",
            "key": "session:sess-1",
            "queued_after_request_id": "req-1"
        }
    }"#;

    assert_eq!(
        parse_queue_hints(Some(metadata)),
        Some(hints(
            QueueSource::BackgroundCompletion,
            QueuePolicy::Coalesce
        ))
    );
}

#[test]
fn parses_all_supported_string_values() {
    let cases = [
        ("user", QueueSource::User),
        ("background_completion", QueueSource::BackgroundCompletion),
        ("subagent_completion", QueueSource::BackgroundCompletion),
        ("steering", QueueSource::Steering),
        ("goal", QueueSource::Goal),
    ];

    for (source, expected_source) in cases {
        let metadata = format!(
            r#"{{
                "queue": {{
                    "source": "{source}",
                    "policy": "append",
                    "key": null,
                    "queued_after_request_id": null
                }}
            }}"#
        );

        assert_eq!(
            parse_queue_hints(Some(&metadata)),
            Some(QueueHints {
                source: expected_source,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
            })
        );
    }

    let metadata = r#"{
        "queue": {
            "source": "user",
            "policy": "coalesce",
            "key": null,
            "queued_after_request_id": null
        }
    }"#;

    assert_eq!(
        parse_queue_hints(Some(metadata)).map(|hints| hints.policy),
        Some(QueuePolicy::Coalesce)
    );
}

#[test]
fn returns_none_for_absent_blank_invalid_or_non_queue_metadata() {
    assert_eq!(parse_queue_hints(None), None);
    assert_eq!(parse_queue_hints(Some("   ")), None);
    assert_eq!(parse_queue_hints(Some("not json")), None);
    assert_eq!(parse_queue_hints(Some(r#"{"run_id":"abc"}"#)), None);
    assert_eq!(
        parse_queue_hints(Some(r#"{"queue":{"source":"timer","policy":"append"}}"#)),
        None
    );
}

#[test]
fn serializes_queue_metadata_json() {
    let json = queue_metadata_json(&QueueHints {
        source: QueueSource::Steering,
        policy: QueuePolicy::Coalesce,
        key: Some("agent:did:key:z123".to_string()),
        queued_after_request_id: None,
        interrupted_request_id: None,
    });

    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "continuation_version": 1,
            "queue": {
                "source": "steering",
                "policy": "coalesce",
                "key": "agent:did:key:z123",
                "queued_after_request_id": null
            }
        })
    );
}

#[test]
fn continuation_policy_matches_the_formal_contract_signatures() {
    assert_eq!(
        ContinuationKind::Steering.contract_signature(),
        "steering|append|interactive|visible_input|runtime_control|generation_owner|durable_input|prompt_once"
    );
    assert_eq!(
        ContinuationKind::BackgroundCompletion.contract_signature(),
        "background_completion|coalesce|scheduled|runtime_control|runtime_control|generation_owner|durable_input|history_then_control"
    );
    assert_eq!(
        ContinuationKind::Goal.contract_signature(),
        "goal|coalesce|scheduled|none|runtime_control|previous_request|request_only|control_only"
    );
}

#[test]
fn legacy_steering_request_projects_as_input_but_versioned_request_is_control() {
    let legacy = r#"{"queue":{"source":"steering","policy":"append","key":null,"queued_after_request_id":null}}"#;
    let versioned = queue_metadata_json(&QueueHints {
        source: QueueSource::Steering,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
    });

    assert_eq!(
        classify_continuation_request(Some(legacy), "legacy steering input"),
        ConversationProjection::VisibleInput
    );
    assert_eq!(
        classify_continuation_request(Some(&versioned), "legacy steering input"),
        ConversationProjection::RuntimeControl
    );
}

#[test]
fn automated_wakeup_is_true_only_for_keyed_subagent_completion_coalesce() {
    assert!(!is_automated_wakeup(None));
    assert!(!is_automated_wakeup(Some(&queue_metadata_json(
        &QueueHints {
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
        }
    ))));
    assert!(is_automated_wakeup(Some(&queue_metadata_json(
        &QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:session-1".to_string()),
            queued_after_request_id: None,
            interrupted_request_id: None,
        }
    ))));
    assert!(!is_automated_wakeup(Some(&queue_metadata_json(
        &QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Append,
            key: Some("background_completion:session-1".to_string()),
            queued_after_request_id: None,
            interrupted_request_id: None,
        }
    ))));
    assert!(!is_automated_wakeup(Some(&queue_metadata_json(
        &QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
        }
    ))));
    assert!(!is_automated_wakeup(Some(&queue_metadata_json(
        &QueueHints {
            source: QueueSource::Steering,
            policy: QueuePolicy::Coalesce,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
        }
    ))));
}

#[test]
fn runtime_control_projection_keeps_only_the_steering_input_visible() {
    let metadata = |source| {
        queue_metadata_json(&QueueHints {
            source,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
        })
    };
    let steering = metadata(QueueSource::Steering);
    let steering_input = steering_input_message_key("request-1");

    assert_eq!(
        classify_continuation_message(
            Some(&steering),
            Some("request-1"),
            Some("steer now"),
            "user",
            "steer now",
            &steering_input
        ),
        ConversationProjection::VisibleInput
    );
    assert_eq!(
        classify_continuation_message(
            Some(&steering),
            Some("request-1"),
            Some("steer now"),
            "user",
            "<context>internal</context>",
            ""
        ),
        ConversationProjection::RuntimeControl
    );
    assert_eq!(
        classify_continuation_message(
            Some(&steering),
            Some("request-1"),
            Some("steer now"),
            "assistant",
            "done",
            "",
        ),
        ConversationProjection::Ordinary
    );
    assert_eq!(
        classify_continuation_message(
            Some(&metadata(QueueSource::Goal)),
            Some("request-2"),
            Some("goal control"),
            "user",
            "goal control",
            ""
        ),
        ConversationProjection::RuntimeControl
    );
    assert_eq!(
        classify_continuation_message(
            None,
            None,
            None,
            "user",
            "notification",
            "background-completion-notification:child-1:subagent"
        ),
        ConversationProjection::RuntimeControl
    );
    assert_eq!(
        classify_continuation_message(
            Some(&metadata(QueueSource::User)),
            Some("request-3"),
            Some("hello"),
            "user",
            "hello",
            ""
        ),
        ConversationProjection::Ordinary
    );
    assert!(!crate::lifecycle::request_content_owns_user_projection(
        Some(&steering)
    ));
    assert!(crate::lifecycle::request_content_owns_user_projection(None));
    assert!(crate::lifecycle::request_owns_user_turn(Some(&steering)));
    assert!(!crate::lifecycle::request_owns_user_turn(Some(&metadata(
        QueueSource::Goal
    ))));
}

#[test]
fn malformed_legacy_steering_policy_remains_request_only_control() {
    let metadata = r#"{"queue":{"source":"steering"}}"#;
    assert!(metadata_is_request_only_control(Some(metadata)));
}
