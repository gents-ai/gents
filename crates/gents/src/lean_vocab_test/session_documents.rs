//! Lean transport projections use interned IDs and logical time. Runtime
//! consumers map them to canonical documents; these are not storage models.
use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSessionDocumentCases {
    pub(crate) document_reference_encoding: String,
    pub(crate) selection: Vec<serde_json::Value>,
    pub(crate) projection: Vec<serde_json::Value>,
    pub(crate) retry: Vec<serde_json::Value>,
    pub(crate) fork: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanBudgetRehydrationCase {
    pub(crate) name: String,
    pub(crate) request_doc_id: String,
    pub(crate) pinned_limit: Option<u64>,
    pub(crate) rows: Vec<LeanDurableUsageRow>,
    pub(crate) ledger: Option<LeanRehydratedBudget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableUsageRow {
    pub(crate) request_doc_id: String,
    pub(crate) kind: String,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRehydratedBudget {
    pub(crate) limit: u64,
    pub(crate) used: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::timeline::{
        build_timeline_order, OverlayInput, OverlayPlacement, PendingInput, PendingPlacement,
        TimelineMessageInput, TimelineRole, TimelineSlot,
    };
    use serde_json::{json, Value};

    #[test]
    fn generated_pending_timeline_cases_preserve_request_identities_and_order() {
        let cases: Vec<_> = lean_contract_snapshot()
            .session_document_cases
            .projection
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
}
