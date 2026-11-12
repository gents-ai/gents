#[cfg(test)]
mod tests {
    use crate::*;
    use gents_protocol::timeline::{
        build_timeline_order, OverlayInput, OverlayPlacement, PendingInput, PendingPlacement,
        TimelineMessageInput, TimelineRole, TimelineSlot,
    };
    use serde_json::{json, Value};

    #[test]
    fn generated_output_obligation_cases_match_runtime_input_classification() {
        use gents_protocol::output::{authored, MessageRole};
        let cases: Vec<_> = lean_contract_snapshot()
            .session_document_projection_cases
            .iter()
            .filter(|case| case["operation"] == "output_obligation_control")
            .collect();
        assert!(!cases.is_empty());
        for case in cases {
            let role = if case["user"].as_bool().unwrap() {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            assert_eq!(
                authored::is_output_obligation(
                    role,
                    case["request"].as_str(),
                    case["key"].as_str().unwrap()
                ),
                case["control"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
        }
        assert_eq!(authored::output_obligation_key(12), "output-obligation:12");
    }

    #[test]
    fn generated_pending_timeline_cases_preserve_request_identities_and_order() {
        let cases: Vec<_> = lean_contract_snapshot()
            .session_document_projection_cases
            .iter()
            .filter(|case| case["operation"] == "pending_timeline")
            .collect();
        assert!(!cases.is_empty());
        for case in cases {
            let messages: Vec<_> = case["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| TimelineMessageInput {
                    key: row["key"].as_u64().unwrap().to_string(),
                    sequence: Some(row["sequence"].as_i64().unwrap()),
                    role: match row["role"].as_str().unwrap() {
                        "user" => TimelineRole::User,
                        "assistant" => TimelineRole::Assistant,
                        role => panic!("unknown modeled role {role}"),
                    },
                    emits_item: row["emits_item"].as_bool().unwrap(),
                    dedup_token: row["token"].as_u64().map(|key| key.to_string()),
                })
                .collect();
            let pending: Vec<_> = case["pending"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| PendingInput {
                    key: row["key"].as_u64().unwrap().to_string(),
                    placement: row["before_sequence"]
                        .as_i64()
                        .map_or(PendingPlacement::Tail, |message_sequence| {
                            PendingPlacement::BeforeMessage { message_sequence }
                        }),
                })
                .collect();
            let groups: Vec<_> = case["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|sequence| Some(sequence.as_i64().unwrap()))
                .collect();
            let overlay = (!case["overlay"].is_null()).then(|| OverlayInput {
                has_durable_owner: case["overlay"]["has_durable_owner"].as_bool().unwrap(),
                placement: case["overlay"]["before_sequence"].as_i64().map_or(
                    OverlayPlacement::Tail,
                    |sequence| OverlayPlacement::BeforeOrphan {
                        message_sequence: Some(sequence),
                    },
                ),
            });
            let actual: Vec<Value> = build_timeline_order(&messages, &groups, &pending, overlay)
                .into_iter().map(|slot| match slot {
                    TimelineSlot::Message { key, sequence, role } => json!({"kind":"message",
                        "key":key.parse::<u64>().unwrap(), "sequence":sequence.unwrap(),
                        "role":match role { TimelineRole::User => "user", TimelineRole::Assistant => "assistant" }}),
                    TimelineSlot::ToolGroup { message_sequence } => json!({"kind":"tool_group", "sequence":message_sequence.unwrap()}),
                    TimelineSlot::Pending { key } => json!({"kind":"pending", "key":key.parse::<u64>().unwrap()}),
                    TimelineSlot::Overlay => json!({"kind":"overlay"}),
                }).collect();
            assert_eq!(json!(actual), case["slots"], "{}", case["name"]);
        }
    }

    #[test]
    fn generated_question_reply_source_cases_preserve_attribution() {
        let cases: Vec<_> = lean_contract_snapshot()
            .session_document_projection_cases
            .iter()
            .filter(|case| case["operation"] == "question_reply_source")
            .collect();
        assert!(!cases.is_empty());
        for case in cases {
            let actual = gents_protocol::question_reply_source::question_reply_source(
                case["person"].as_bool().unwrap(),
                case["request_source"].as_str(),
                case["evidence_source"].as_str(),
                case["observed_question"].as_bool().unwrap(),
            );
            assert_eq!(actual, case["source"].as_str(), "{}", case["name"]);
        }
    }
}
