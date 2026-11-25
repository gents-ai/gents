use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A stage field or capture kind a case author writes beside a stage's
/// checks, with what each way the stage ends means for its checks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StageFieldDescription {
    pub name: String,
    pub applies_to: String,
    /// JSON Schema of the field as written in a case, from the case type.
    pub schema: Value,
    pub outcomes: Vec<(String, String)>,
}

pub fn stage_fields() -> Vec<StageFieldDescription> {
    let lines = |lines: &[(&str, &str)]| {
        lines
            .iter()
            .map(|(code, line)| (code.to_string(), line.to_string()))
            .collect()
    };
    vec![
        StageFieldDescription {
            name: "expect_fire".into(),
            applies_to: "a seed stage".into(),
            schema: gents_documents::eval_stage_field_schema("expect_fire").unwrap_or_default(),
            outcomes: lines(&[
                ("observed", "true and a request names the seed as its cause: that request decides the stage"),
                ("passed", "false, nothing fired for the seed, and every request of the trial home stayed terminal with none added for 60 s; the stage's checks then run"),
                ("model_acceptance", "true and nothing fired for the seed within deadline_secs (600 s when absent), or false and a request fired for it; the subject's failure"),
                ("runtime", "a trigger fire for the seed recorded error or budget_exhausted; the trigger's last_error is the verdict detail"),
                ("inconclusive", "false and the trial home was not quiet before deadline_secs ended"),
                ("infrastructure", "the trial runtime stopped, a read failed, or the only failed fire was unacknowledged; not evidence"),
            ]),
        },
        StageFieldDescription {
            name: "capture kind runs".into(),
            applies_to: "any stage; of_seed needs a seed stage".into(),
            schema: gents_documents::eval_capture_kind_schema("runs").unwrap_or_default(),
            outcomes: lines(&[
                ("rows", "one row per trigger-fired request that came into being while the stage ran, whoever started it: request_id, trigger_id, source_collection, source_doc_id, session_id, lifecycle_state, final_message, tool_results [{tool_name, status, result}]"),
                ("checks", "read like a documents capture: captured_rows_count counts runs, captured_fields_match grades their fields; settle the stage so later links finish first"),
            ]),
        },
        StageFieldDescription {
            name: "stage_failed detail".into(),
            applies_to: "every verdict of a failed stage".into(),
            schema: serde_json::json!({"type": "string"}),
            outcomes: lines(&[(
                "detail",
                "raw.detail names what ended the stage when its request's record does not: a trigger's recorded error, a seed nothing fired for, or a seed that fired when it had to be skipped",
            )]),
        },
    ]
}
