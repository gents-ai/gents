//! `captured_fields_match`: whether a documents capture's rows hold the
//! expected field values, graded per row and expectation.

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::{
    excerpt, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
    EXCERPT_CHARS,
};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};

/// Params: `{ "name": <capture>, "expect": [{ "field", "equals" | "contains"
/// | "matches" }], "min_rows": <u64>?, "max_rows": <u64>? }`. Every (row,
/// expectation) pair is one requirement, over at least `min_rows` rows
/// (default 1): a row the capture lacks fails every expectation, and so does
/// every row past `max_rows`. `field` is a dotted path of object keys into
/// the row; `contains` and `matches` read a string field as its text and any
/// other value as its JSON.
///
/// Feedback states the expected values. It is written only on the train
/// split, and the held-out split is what catches a candidate that hard-codes
/// them.
pub struct CapturedFieldsMatch;

/// Rows whose mismatches feedback spells out.
const SHOWN_ROWS: usize = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    name: String,
    expect: Vec<Expectation>,
    #[serde(default = "one")]
    min_rows: usize,
    #[serde(default)]
    max_rows: Option<usize>,
}

fn one() -> usize {
    1
}

/// A present `equals: null` is a test for null, not an absent test.
fn some<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expectation {
    pub(super) field: String,
    #[serde(default)]
    fallback_field: Option<String>,
    #[serde(default, deserialize_with = "some")]
    equals: Option<Value>,
    #[serde(default)]
    contains: Option<String>,
    #[serde(default)]
    matches: Option<String>,
}

#[derive(Clone)]
pub(super) struct FieldPath {
    field: String,
    fallback_field: Option<String>,
}

impl FieldPath {
    /// A missing or null primary value uses the fallback. Empty strings and
    /// other non-null values retain precedence, matching nullable UI labels.
    pub(super) fn resolve(&self, mut lookup: impl FnMut(&str) -> Option<Value>) -> Option<Value> {
        let actual = lookup(&self.field);
        match &self.fallback_field {
            Some(fallback) if actual.as_ref().is_none_or(Value::is_null) => lookup(fallback),
            _ => actual,
        }
    }
}

impl std::fmt::Display for FieldPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.field)?;
        if let Some(fallback) = &self.fallback_field {
            write!(f, " (or {fallback} when absent or null)")?;
        }
        Ok(())
    }
}

pub(super) enum Test {
    Equals(Value),
    Contains(String),
    Matches(Regex),
}

impl Test {
    pub(super) fn holds(&self, actual: &Value) -> bool {
        match self {
            Self::Equals(expected) => actual == expected,
            Self::Contains(needle) => text(actual).contains(needle.as_str()),
            Self::Matches(pattern) => pattern.is_match(&text(actual)),
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            Self::Equals(expected) => format!("equals {expected}"),
            Self::Contains(needle) => format!("contains {needle:?}"),
            Self::Matches(pattern) => format!("matches /{pattern}/"),
        }
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `expectation` as a test, or why it is not one.
pub(super) fn test(expectation: Expectation) -> Result<(FieldPath, Test), String> {
    let field = FieldPath {
        field: expectation.field,
        fallback_field: expectation.fallback_field,
    };
    match (
        expectation.equals,
        expectation.contains,
        expectation.matches,
    ) {
        (Some(expected), None, None) => Ok((field, Test::Equals(expected))),
        (None, Some(needle), None) => Ok((field, Test::Contains(needle))),
        (None, None, Some(pattern)) => Regex::new(&pattern)
            .map(|pattern| (field.clone(), Test::Matches(pattern)))
            .map_err(|error| format!("field {field}: {error}")),
        _ => Err(format!(
            "field {field}: give exactly one of equals, contains or matches"
        )),
    }
}

fn lookup<'a>(row: &'a Value, field: &str) -> Option<&'a Value> {
    field.split('.').try_fold(row, |value, key| value.get(key))
}

impl Check for CapturedFieldsMatch {
    fn name(&self) -> &'static str {
        "captured_fields_match"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn describe(&self) -> CheckDescription {
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: "Scores the fraction of (row, expectation) pairs that hold over a documents capture's rows, each pair counting once, so larger captures weigh more. Rows below min_rows (default 1) or past max_rows fail every expectation.".into(),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "the capture name"},
                    "expect": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "field": {"type": "string", "description": "dotted path of object keys into the row; no array indexing"},
                                "fallback_field": {"type": "string", "description": "path used only when field is absent or null"},
                                "equals": {},
                                "contains": {"type": "string"},
                                "matches": {"type": "string", "description": "a regex"}
                            },
                            "required": ["field"],
                            "oneOf": [
                                {"required": ["equals"]},
                                {"required": ["contains"]},
                                {"required": ["matches"]}
                            ],
                            "additionalProperties": false
                        }
                    },
                    "min_rows": {"type": "integer", "minimum": 0},
                    "max_rows": {"type": ["integer", "null"], "minimum": 0}
                },
                "required": ["name", "expect"],
                "additionalProperties": false
            }),
            reads: vec!["capture:documents".into()],
            reason_codes: graded_reason_codes(&[(
                "missing_capture",
                "grader: no documents capture of that name",
            )]),
        }
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string()),
        };
        if params.expect.is_empty() {
            return grader("bad_params", "expect names no field");
        }
        let tests = match params
            .expect
            .into_iter()
            .map(test)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(tests) => tests,
            Err(detail) => return grader("bad_params", detail),
        };
        let max_rows = params.max_rows.unwrap_or(usize::MAX);
        if max_rows < params.min_rows {
            return grader(
                "bad_params",
                format!("max_rows {max_rows} is below min_rows {}", params.min_rows),
            );
        }
        let name = &params.name;
        let rows = match stage.captures.get(name) {
            Some(CaptureResult::Documents { rows }) => rows,
            Some(CaptureResult::Files { .. } | CaptureResult::Schema { .. }) => {
                return grader(
                    "missing_capture",
                    format!("capture {name} does not hold documents"),
                )
            }
            None => {
                return grader(
                    "missing_capture",
                    format!("the stage produced no capture named {name}"),
                )
            }
        };
        let row_count = rows.len().max(params.min_rows);
        let total = tests.len() * row_count;
        if total == 0 {
            return graded(1, 1, None);
        }
        let mut satisfied = 0;
        let mut lines = Vec::new();
        let mut hidden = 0;
        for (index, row) in rows.iter().take(max_rows).enumerate() {
            let mut mismatched = false;
            for (field, test) in &tests {
                let actual = field.resolve(|path| lookup(row, path).cloned());
                if actual.as_ref().is_some_and(|actual| test.holds(actual)) {
                    satisfied += 1;
                    continue;
                }
                mismatched = true;
                if index < SHOWN_ROWS {
                    let got = actual.map_or("absent".to_string(), |actual| {
                        excerpt(&actual.to_string(), EXCERPT_CHARS)
                    });
                    lines.push(format!(
                        "row {index}: {field} expected {}, got {got}",
                        excerpt(&test.describe(), EXCERPT_CHARS)
                    ));
                }
            }
            hidden += usize::from(mismatched && index >= SHOWN_ROWS);
        }
        if hidden > 0 {
            lines.push(format!("… {hidden} more rows with a mismatch"));
        }
        if rows.len() < params.min_rows {
            lines.push(format!(
                "missing rows: {name} holds {} rows, {} required; rows {}..{} absent",
                rows.len(),
                params.min_rows,
                rows.len(),
                params.min_rows - 1
            ));
        }
        if rows.len() > max_rows {
            lines.push(format!(
                "extra rows: {name} holds {} rows, at most {max_rows} allowed",
                rows.len()
            ));
        }
        let feedback = (!lines.is_empty()).then(|| lines.join("\n"));
        graded(satisfied, total, feedback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;

    /// One completed stage whose `items` capture holds `rows`.
    fn stage(rows: Vec<Value>) -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", "items", rows);
        evidence.stages.remove(0)
    }

    fn feedback(verdict: &CheckVerdict) -> &str {
        verdict.feedback.as_deref().unwrap_or_default()
    }

    #[test]
    fn nullable_labels_fall_back_without_overriding_explicit_names() {
        let params = json!({"name":"items","expect":[{"field":"display_name","fallback_field":"profile_id","equals":"Research"}]});
        for (row, met) in [
            (json!({"profile_id":"Research"}), true),
            (json!({"profile_id":"Research","display_name":null}), true),
            (
                json!({"profile_id":"other","display_name":"Research"}),
                true,
            ),
            (
                json!({"profile_id":"Research","display_name":"Wrong"}),
                false,
            ),
            (json!({"profile_id":"Research","display_name":""}), false),
            (json!({"display_name":null}), false),
        ] {
            let verdict = CapturedFieldsMatch.evaluate(&params, &stage(vec![row.clone()]));
            assert_eq!(
                verdict.score_bp == Some(10000),
                met,
                "{row}: {:?}",
                verdict.feedback
            );
        }
    }

    #[test]
    fn rows_that_hold_every_expectation_pass_with_no_feedback() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [
                {"field": "sku", "equals": "A1"},
                {"field": "note", "contains": "disk"},
                {"field": "payload.level", "matches": "^(high|critical)$"}
            ]}),
            &stage(vec![
                json!({"sku": "A1", "note": "disk full", "payload": {"level": "high"}}),
            ]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Passed, Some(10_000))
        );
        assert_eq!(verdict.feedback, None);
    }

    #[test]
    fn the_score_counts_every_row_and_expectation_pair() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}, {"field": "qty", "equals": 2}]}),
            &stage(vec![json!({"sku": "A1", "qty": 2}), json!({"sku": "B2", "qty": 2})]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(7_500))
        );
        let text = feedback(&verdict);
        assert!(
            text.contains(r#"row 1: sku expected equals "A1", got "B2""#),
            "{text}"
        );
        assert!(!text.contains("row 0"), "only what is missing: {text}");
    }

    /// The schema admits `equals: null`, so the parser must read it as a test
    /// for null rather than as no test at all.
    #[test]
    fn equals_null_tests_for_a_null_value() {
        let params = json!({"name": "items", "expect": [{"field": "sku", "equals": null}]});
        let verdict = CapturedFieldsMatch.evaluate(&params, &stage(vec![json!({"sku": null})]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Passed, Some(10_000))
        );
        let verdict = CapturedFieldsMatch.evaluate(&params, &stage(vec![json!({"sku": "A1"})]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(0))
        );
        assert!(feedback(&verdict).contains("sku expected equals null, got \"A1\""));
    }

    #[test]
    fn an_absent_field_is_reported_as_absent() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "contains": "A"}]}),
            &stage(vec![json!({})]),
        );
        assert_eq!(verdict.score_bp, Some(0));
        assert!(feedback(&verdict).contains("row 0: sku expected contains \"A\", got absent"));
    }

    #[test]
    fn rows_below_min_rows_fail_every_expectation_and_are_named_missing() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}], "min_rows": 3}),
            &stage(vec![json!({"sku": "A1"})]),
        );
        assert_eq!(verdict.score_bp, Some(3_333));
        let text = feedback(&verdict);
        assert!(
            text.contains("items holds 1 rows, 3 required; rows 1..2 absent"),
            "{text}"
        );

        let empty = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}]}),
            &stage(vec![]),
        );
        assert_eq!(empty.score_bp, Some(0), "min_rows defaults to 1");
        let vacuous = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}], "min_rows": 0}),
            &stage(vec![]),
        );
        assert_eq!(vacuous.score_bp, Some(10_000));
    }

    /// Per-pair scoring would otherwise reward padding a capture with rows
    /// that happen to match.
    #[test]
    fn rows_beyond_max_rows_count_as_unmet_pairs() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}], "max_rows": 1}),
            &stage(vec![
                json!({"sku": "A1"}),
                json!({"sku": "A1"}),
                json!({"sku": "A1"}),
            ]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(3_333))
        );
        let text = feedback(&verdict);
        assert!(
            text.contains("extra rows: items holds 3 rows, at most 1 allowed"),
            "{text}"
        );
        let bad = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}], "min_rows": 2, "max_rows": 1}),
            &stage(vec![]),
        );
        assert_eq!(bad.raw["reason_code"], "bad_params");
    }

    #[test]
    fn feedback_details_the_first_rows_and_counts_the_rest() {
        let rows = (0..50).map(|_| json!({"sku": "x".repeat(1_000)})).collect();
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": "A1"}]}),
            &stage(rows),
        );
        let text = feedback(&verdict);
        assert!(text.len() <= 2_048, "{}", text.len());
        assert!(
            text.contains("row 2:") && !text.contains("row 3:"),
            "{text}"
        );
        assert!(text.contains("47 more rows with a mismatch"), "{text}");
    }

    #[test]
    fn a_missing_or_files_capture_is_a_grader_outcome() {
        let verdict = CapturedFieldsMatch.evaluate(
            &json!({"name": "other", "expect": [{"field": "sku", "equals": "A1"}]}),
            &stage(vec![]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
            (OutcomeKind::Grader, None, &json!("missing_capture"))
        );
    }

    #[test]
    fn malformed_expectations_are_bad_params() {
        for params in [
            json!({"name": "items", "expect": []}),
            json!({"name": "items", "expect": [{"field": "sku"}]}),
            json!({"name": "items", "expect": [{"field": "sku", "equals": 1, "contains": "1"}]}),
            json!({"name": "items", "expect": [{"field": "sku", "matches": "("}]}),
            json!({"name": "items", "expect": [{"field": "sku", "equals": 1}], "extra": 1}),
        ] {
            let verdict = CapturedFieldsMatch.evaluate(&params, &stage(vec![json!({})]));
            assert_eq!(
                (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
                (OutcomeKind::Grader, None, &json!("bad_params")),
                "{params}"
            );
        }
    }

    #[test]
    fn the_schema_requires_exactly_one_test_per_expectation() {
        let validator =
            jsonschema::validator_for(&CapturedFieldsMatch.describe().params_schema).unwrap();
        assert!(validator.is_valid(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": null}], "min_rows": 2})
        ));
        assert!(!validator.is_valid(&json!({"name": "items", "expect": [{"field": "sku"}]})));
        assert!(!validator.is_valid(
            &json!({"name": "items", "expect": [{"field": "sku", "equals": 1, "matches": "1"}]})
        ));
        assert!(!validator.is_valid(&json!({"name": "items", "expect": []})));
    }
}
