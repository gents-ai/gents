#![allow(dead_code)] // Introduced ahead of the Rust queue scheduler tasks that consume it.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequestQueueMetadata {
    pub queue: QueueHints,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueueHints {
    pub source: QueueSource,
    pub policy: QueuePolicy,
    pub key: Option<String>,
    pub queued_after_request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueueSource {
    User,
    SubagentCompletion,
    Steering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueuePolicy {
    Append,
    Coalesce,
}

pub(crate) fn parse_queue_hints(metadata: Option<&str>) -> Option<QueueHints> {
    let metadata = metadata?.trim();
    if metadata.is_empty() {
        return None;
    }

    serde_json::from_str::<RequestQueueMetadata>(metadata)
        .ok()
        .map(|metadata| metadata.queue)
}

pub(crate) fn queue_metadata_json(hints: &QueueHints) -> String {
    serde_json::to_string(&RequestQueueMetadata {
        queue: hints.clone(),
    })
    .expect("queue metadata serialization should not fail")
}

pub(crate) fn is_automated_wakeup(metadata: Option<&str>) -> bool {
    parse_queue_hints(metadata).is_some_and(|hints| {
        matches!(hints.source, QueueSource::SubagentCompletion)
            && hints.policy == QueuePolicy::Coalesce
            && hints
                .key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(source: QueueSource, policy: QueuePolicy) -> QueueHints {
        QueueHints {
            source,
            policy,
            key: Some("session:sess-1".to_string()),
            queued_after_request_id: Some("req-1".to_string()),
        }
    }

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
                QueueSource::SubagentCompletion,
                QueuePolicy::Coalesce
            ))
        );
    }

    #[test]
    fn parses_all_supported_string_values() {
        let cases = [
            ("user", QueueSource::User),
            ("subagent_completion", QueueSource::SubagentCompletion),
            ("steering", QueueSource::Steering),
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
        });

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
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
    fn automated_wakeup_is_true_only_for_keyed_subagent_completion_coalesce() {
        assert!(!is_automated_wakeup(None));
        assert!(!is_automated_wakeup(Some(&queue_metadata_json(
            &QueueHints {
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: None,
            }
        ))));
        assert!(is_automated_wakeup(Some(&queue_metadata_json(
            &QueueHints {
                source: QueueSource::SubagentCompletion,
                policy: QueuePolicy::Coalesce,
                key: Some("subagent_completion:session-1".to_string()),
                queued_after_request_id: None,
            }
        ))));
        assert!(!is_automated_wakeup(Some(&queue_metadata_json(
            &QueueHints {
                source: QueueSource::SubagentCompletion,
                policy: QueuePolicy::Append,
                key: Some("subagent_completion:session-1".to_string()),
                queued_after_request_id: None,
            }
        ))));
        assert!(!is_automated_wakeup(Some(&queue_metadata_json(
            &QueueHints {
                source: QueueSource::SubagentCompletion,
                policy: QueuePolicy::Coalesce,
                key: None,
                queued_after_request_id: None,
            }
        ))));
        assert!(!is_automated_wakeup(Some(&queue_metadata_json(
            &QueueHints {
                source: QueueSource::Steering,
                policy: QueuePolicy::Coalesce,
                key: None,
                queued_after_request_id: None,
            }
        ))));
    }
}
