pub mod captured_fields_match;
pub mod captured_rows_count;
pub mod crew_spec_match;
pub mod final_message_matches;
pub mod handoff_delivery;
pub mod schema_matches;
pub mod tool_calls_expected;
pub mod tool_result_matches;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::eval::checks::captured_fields_match::CapturedFieldsMatch;
use crate::eval::checks::captured_rows_count::CapturedRowsCount;
use crate::eval::checks::crew_spec_match::CrewSpecMatch;
use crate::eval::checks::final_message_matches::FinalMessageMatches;
use crate::eval::checks::handoff_delivery::HandoffDelivery;
use crate::eval::checks::tool_calls_expected::ToolCallsExpected;
use crate::eval::runner::executor::StageEvidence;
use crate::eval::OutcomeKind;

/// Frozen into every run's origin; bump when the builtin set changes in a way that could move a score.
pub const CHECK_REGISTRY_VERSION: &str = "4";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckVerdict {
    pub kind: OutcomeKind,
    pub score_bp: Option<u32>,
    pub raw: Value,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckDescription {
    pub name: String,
    pub version: String,
    pub summary: String,
    /// JSON Schema (draft 2020-12) for the check ref's `params`.
    pub params_schema: Value,
    pub reads: Vec<String>,
    pub reason_codes: Vec<(String, String)>,
}

pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;

    fn version(&self) -> &'static str;

    /// Pure: malformed params are a grader outcome, never a panic.
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;

    fn describe(&self) -> CheckDescription;
}

pub struct CheckRegistry {
    checks: BTreeMap<&'static str, Box<dyn Check>>,
}

impl CheckRegistry {
    pub fn builtin() -> Self {
        let mut registry = Self {
            checks: BTreeMap::new(),
        };
        registry.register(Box::new(CapturedFieldsMatch));
        registry.register(Box::new(CapturedRowsCount));
        registry.register(Box::new(CrewSpecMatch));
        registry.register(Box::new(FinalMessageMatches));
        registry.register(Box::new(HandoffDelivery));
        registry.register(Box::new(ToolCallsExpected));
        registry.register(Box::new(tool_result_matches::ToolResultMatches));
        registry.register(Box::new(schema_matches::SchemaMatches));
        registry
    }

    pub fn get(&self, name: &str) -> Option<&dyn Check> {
        self.checks.get(name).map(AsRef::as_ref)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.checks.keys().copied().collect()
    }

    pub fn catalog(&self) -> Vec<CheckDescription> {
        self.checks.values().map(|check| check.describe()).collect()
    }

    #[cfg(test)]
    pub(crate) fn with(mut self, check: Box<dyn Check>) -> Self {
        self.register(check);
        self
    }

    fn register(&mut self, check: Box<dyn Check>) {
        self.checks.insert(check.name(), check);
    }
}

impl Default for CheckRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

pub fn verdict_detail(raw: &Value) -> Option<String> {
    let number = |key: &str| raw.get(key).and_then(Value::as_u64);
    if let (Some(observed), Some(expected)) = (number("observed"), raw.get("expected")) {
        let min = expected.get("min").and_then(Value::as_u64).unwrap_or(0);
        let max = expected.get("max").and_then(Value::as_u64);
        let subject = raw
            .get("capture")
            .and_then(Value::as_str)
            .map_or_else(String::new, |capture| format!("{capture} "));
        return Some(format!(
            "{subject}observed {observed} expected {}",
            range_label(min, max)
        ));
    }
    if let (Some(satisfied), Some(total)) = (number("satisfied"), number("total")) {
        return Some(format!("{satisfied} of {total} requirements met"));
    }
    raw.get("detail")
        .and_then(Value::as_str)
        .map(|detail| excerpt(detail, EXCERPT_CHARS))
}

pub fn range_label(min: u64, max: Option<u64>) -> String {
    match max {
        None => format!("≥{min}"),
        Some(max) if max == min => max.to_string(),
        Some(max) if min == 0 => format!("≤{max}"),
        Some(max) => format!("{min}..{max}"),
    }
}

const EXCERPT_CHARS: usize = 120;

/// Each verdict's feedback is rendered into the proposer's prompt once, so it is capped.
const FEEDBACK_BYTES: usize = 2048;

fn excerpt(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

fn graded(satisfied: usize, total: usize, feedback: Option<String>) -> CheckVerdict {
    let met = satisfied == total;
    CheckVerdict {
        kind: if met {
            OutcomeKind::Passed
        } else {
            OutcomeKind::ModelAcceptance
        },
        score_bp: Some((satisfied * 10_000 / total) as u32),
        raw: json!({
            "reason_code": if met { "met" } else { "unmet" },
            "satisfied": satisfied,
            "total": total,
        }),
        feedback: feedback.map(bounded),
    }
}

fn bounded(mut text: String) -> String {
    if text.len() > FEEDBACK_BYTES {
        text.truncate(text.floor_char_boundary(FEEDBACK_BYTES - "…".len()));
        text.push('…');
    }
    text
}

pub(crate) fn failure_feedback(stage: &StageEvidence) -> String {
    let mut text = format!(
        "stage ended {}",
        stage
            .terminal_state
            .map_or("unknown", |state| state.as_str())
    );
    if let Some(kind) = stage.failure_kind {
        text.push_str(&format!("; failure_kind {}", kind.as_str()));
    }
    if let Some(reason) = stage.provider_reason {
        text.push_str(&format!("; provider_reason {}", reason.as_str()));
    }
    let last_error =
        stage.tool_calls.iter().rev().find(|call| {
            call.status.as_deref() == Some("failed") || call.tool_failure_class.is_some()
        });
    if let Some(call) = last_error {
        let message = match &call.result {
            Value::String(result) => result.clone(),
            Value::Null => call.tool_failure_class.clone().unwrap_or_default(),
            result => result.to_string(),
        };
        text.push_str(&format!(
            "; last tool error: {}: {}",
            call.tool_name,
            excerpt(&message, EXCERPT_CHARS)
        ));
    }
    bounded(text)
}

fn grader(reason_code: &str, detail: impl Into<String>) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Grader,
        score_bp: None,
        raw: json!({ "reason_code": reason_code, "detail": detail.into() }),
        feedback: None,
    }
}

fn graded_reason_codes(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    [
        ("met", "every requirement held; score 10000"),
        (
            "unmet",
            "some requirement failed; score is satisfied / total in basis points",
        ),
        (
            "bad_params",
            "grader: params did not parse or require nothing",
        ),
    ]
    .iter()
    .chain(extra)
    .map(|(code, line)| (code.to_string(), line.to_string()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_registry_holds_the_seed_check_under_its_own_name() {
        let registry = CheckRegistry::builtin();
        assert_eq!(
            registry.names(),
            vec![
                "captured_fields_match",
                "captured_rows_count",
                "crew_spec_match",
                "final_message_matches",
                "handoff_delivery",
                "schema_matches",
                "tool_calls_expected",
                "tool_result_matches"
            ]
        );
        let check = registry.get("captured_rows_count").expect("the seed check");
        assert_eq!(
            (check.name(), check.version()),
            ("captured_rows_count", "2")
        );
        assert!(registry.get("no_such_check").is_none());
    }

    #[test]
    fn failure_feedback_names_the_failure_and_quotes_the_last_tool_error_bounded() {
        use crate::eval::runner::embedded::observe::ToolCallEvidence;
        use crate::eval::runner::scripted::ScriptedExecutor;
        use crate::eval::ProviderReason;

        let call = |name: &str, status: &str, result: String| ToolCallEvidence {
            tool_name: name.into(),
            status: Some(status.into()),
            lifecycle_state: None,
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
            args: Value::Null,
            result: Value::String(result),
        };
        let mut stage = ScriptedExecutor::failed_evidence(
            "did:x",
            "s1",
            OutcomeKind::Provider,
            Some(ProviderReason::Rejected),
        )
        .stages
        .remove(0);
        stage.tool_calls = vec![
            call("read", "failed", "first error".into()),
            call(
                "write",
                "failed",
                format!("permission denied {}", "x".repeat(5_000)),
            ),
            call("list", "completed", "ok".into()),
        ];
        let text = failure_feedback(&stage);
        assert!(
            text.starts_with("stage ended failed; failure_kind provider; provider_reason rejected"),
            "{text}"
        );
        assert!(
            text.contains("last tool error: write: permission denied"),
            "{text}"
        );
        assert!(!text.contains("first error"), "{text}");
        assert!(text.len() < 400, "{}", text.len());
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn every_builtin_check_describes_itself_with_a_schema_and_reason_codes() {
        let catalog = CheckRegistry::builtin().catalog();
        assert_eq!(
            catalog.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            CheckRegistry::builtin().names()
        );
        for check in &catalog {
            assert_eq!(check.params_schema["type"], "object", "{}", check.name);
            assert!(!check.summary.is_empty(), "{}", check.name);
            assert!(!check.reason_codes.is_empty(), "{}", check.name);
        }
    }

    #[test]
    fn captured_rows_count_schema_accepts_its_params_and_rejects_unknown_fields() {
        let schema = CheckRegistry::builtin()
            .get("captured_rows_count")
            .unwrap()
            .describe()
            .params_schema;
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items"})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "extra": 1})));
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": 3})));
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": null})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": -1})));
    }
}
