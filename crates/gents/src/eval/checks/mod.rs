//! The builtin check registry.
//!
//! An eval definition names a check; it never carries code. A [`Check`] is a
//! pure function from a check ref's params and one stage's evidence to a
//! [`CheckVerdict`], so grading a trial is reproducible from its evidence
//! alone.

pub mod captured_fields_match;
pub mod captured_rows_count;
pub mod crew_spec_match;
pub mod final_message_matches;
pub mod handoff_delivery;
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

/// Bumped when the builtin set changes in a way that could move a score.
/// Frozen into every run's origin.
pub const CHECK_REGISTRY_VERSION: &str = "3";

/// What one check concluded about one stage. `score_bp` is `None` when the
/// verdict is not evidence about the subject, such as a grader fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckVerdict {
    pub kind: OutcomeKind,
    pub score_bp: Option<u32>,
    /// An object carrying at least a `reason_code`.
    pub raw: Value,
    pub feedback: Option<String>,
}

/// What a check tells an author about itself. Rendered into the catalog an
/// eval author drafts against; never read by `evaluate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckDescription {
    pub name: String,
    pub version: String,
    pub summary: String,
    /// JSON Schema (draft 2020-12) for the check ref's `params`.
    pub params_schema: Value,
    /// What the check reads: `"capture:documents"`, `"capture:files"`,
    /// `"stage:terminal_state"`, or a field path such as `"rows.payload"`.
    pub reads: Vec<String>,
    /// `(reason_code, one line)` for every code `raw.reason_code` can carry.
    pub reason_codes: Vec<(String, String)>,
}

pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;

    /// Bumped when the same params would yield a different verdict.
    fn version(&self) -> &'static str;

    /// Pure. `params` is the check ref's params; `stage` is the stage's
    /// evidence. Never panics on either: malformed params are a grader
    /// outcome.
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;

    /// The check's entry in the author-facing catalog.
    fn describe(&self) -> CheckDescription;
}

/// The checks a definition may name, by name.
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
        registry
    }

    pub fn get(&self, name: &str) -> Option<&dyn Check> {
        self.checks.get(name).map(AsRef::as_ref)
    }

    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.checks.keys().copied().collect()
    }

    /// Every registered check's description, sorted by name.
    pub fn catalog(&self) -> Vec<CheckDescription> {
        self.checks.values().map(|check| check.describe()).collect()
    }

    /// Registers `check` over the builtin set. Test-only: the shipped
    /// registry is the builtin one, and a definition may only name a check
    /// that ships with it.
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

/// A verdict's observed and expected values in one line, read from its
/// `raw`: `items observed 7 expected ≥9` for a counted verdict,
/// `2 of 3 requirements met` for a graded one, else the check's own
/// `detail`, cut short. `None` when `raw` carries none of them.
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

/// A row range for a person: `≥9`, `3`, `≤4` or `2..5`.
pub fn range_label(min: u64, max: Option<u64>) -> String {
    match max {
        None => format!("≥{min}"),
        Some(max) if max == min => max.to_string(),
        Some(max) if min == 0 => format!("≤{max}"),
        Some(max) => format!("{min}..{max}"),
    }
}

/// The longest excerpt of evidence a check quotes, in chars.
const EXCERPT_CHARS: usize = 120;

/// Feedback is rendered into the proposer's prompt once per verdict, so each
/// is capped.
const FEEDBACK_BYTES: usize = 2048;

/// `text` cut to `max` chars, marked when cut.
fn excerpt(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// `satisfied` of `total` requirements held, scored in proportion. `total`
/// is never zero: a check with nothing to require rejects its params.
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

/// Why `stage` did not complete: its end state, failure kind, provider
/// reason and last tool error, bounded like any check's feedback.
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

/// The check could not reach a verdict, which is no evidence about the
/// subject.
fn grader(reason_code: &str, detail: impl Into<String>) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Grader,
        score_bp: None,
        raw: json!({ "reason_code": reason_code, "detail": detail.into() }),
        feedback: None,
    }
}

/// The reason codes every graded check shares.
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
    fn a_verdict_detail_names_what_was_observed_against_what_was_expected() {
        let counted = json!({
            "reason_code": "below_min", "capture": "behaviors", "observed": 7,
            "expected": {"min": 9, "max": null}, "count": 7
        });
        assert_eq!(
            verdict_detail(&counted).as_deref(),
            Some("behaviors observed 7 expected ≥9")
        );
        let graded = json!({"reason_code": "unmet", "satisfied": 2, "total": 3});
        assert_eq!(
            verdict_detail(&graded).as_deref(),
            Some("2 of 3 requirements met")
        );
        let older = json!({"reason_code": "below_min", "detail": "items holds 1 rows"});
        assert_eq!(
            verdict_detail(&older).as_deref(),
            Some("items holds 1 rows")
        );
        assert_eq!(verdict_detail(&json!({"reason_code": "met"})), None);
        assert_eq!(
            [
                range_label(9, None),
                range_label(3, Some(3)),
                range_label(0, Some(4)),
                range_label(2, Some(5))
            ],
            ["≥9", "3", "≤4", "2..5"]
        );
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
        // `max` is optional, and `null` means absent, as the params type
        // reads it.
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": 3})));
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": null})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "max": -1})));
    }
}
