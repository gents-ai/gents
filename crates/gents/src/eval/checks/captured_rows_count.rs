//! `captured_rows_count`: how many rows a documents capture holds.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};
use crate::eval::OutcomeKind;

/// Params: `{ "name": "<capture name>", "min": <u64>, "max": <u64 | absent> }`.
/// Passes when `min <= rows <= max`, with no upper bound when `max` is absent.
///
/// The `reason_code` in `raw` is the contract on the pass path: `in_range`
/// when the count satisfies the params, `below_min` under `min`, `above_max`
/// over `max`. Those three are the only outcomes that say anything about the
/// subject; everything else this check emits (`missing_capture`,
/// `bad_params`) is about the grader.
pub struct CapturedRowsCount;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    name: String,
    min: u64,
    #[serde(default)]
    max: Option<u64>,
}

impl Check for CapturedRowsCount {
    fn name(&self) -> &'static str {
        "captured_rows_count"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string(), None),
        };
        // A range no count can satisfy is a mistake in the case, not a failure
        // of the subject: every trial would score zero and read as evidence
        // that the subject did the wrong thing.
        if let Some(max) = params.max.filter(|max| *max < params.min) {
            return grader(
                "bad_params",
                format!(
                    "max {max} is below min {}, so no row count can pass",
                    params.min
                ),
                None,
            );
        }
        let rows = match stage.captures.get(&params.name) {
            Some(CaptureResult::Documents { rows }) => rows.len() as u64,
            Some(CaptureResult::Files { .. }) => {
                return grader(
                    "missing_capture",
                    format!("capture {} holds files, not documents", params.name),
                    None,
                )
            }
            None => {
                return grader(
                    "missing_capture",
                    format!("the stage produced no capture named {}", params.name),
                    None,
                )
            }
        };
        let name = &params.name;
        if rows < params.min {
            return failed(
                "below_min",
                format!(
                    "{name} holds {rows} rows, fewer than the {} required",
                    params.min
                ),
                rows,
            );
        }
        match params.max {
            Some(max) if rows > max => failed(
                "above_max",
                format!("{name} holds {rows} rows, more than the {max} allowed"),
                rows,
            ),
            _ => CheckVerdict {
                kind: OutcomeKind::Passed,
                score_bp: Some(10_000),
                raw: raw("in_range", format!("{name} holds {rows} rows"), Some(rows)),
                feedback: None,
            },
        }
    }
}

/// The subject produced the wrong number of rows.
fn failed(reason_code: &str, detail: String, rows: u64) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::ModelAcceptance,
        score_bp: Some(0),
        raw: raw(reason_code, detail, Some(rows)),
        feedback: None,
    }
}

/// The check itself could not reach a verdict, which is no evidence about the
/// subject.
fn grader(reason_code: &str, detail: String, rows: Option<u64>) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Grader,
        score_bp: None,
        raw: raw(reason_code, detail, rows),
        feedback: None,
    }
}

fn raw(reason_code: &str, detail: String, count: Option<u64>) -> Value {
    json!({ "reason_code": reason_code, "detail": detail, "count": count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;
    use serde_json::json;

    /// One completed stage whose `items` capture holds `rows`.
    fn stage(rows: Vec<Value>) -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", "items", rows);
        evidence.stages.remove(0)
    }

    #[test]
    fn a_row_count_inside_the_range_passes() {
        let verdict = CapturedRowsCount.evaluate(
            &json!({"name": "items", "min": 2, "max": 3}),
            &stage(vec![json!({}), json!({})]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Passed, Some(10000))
        );
        assert_eq!(verdict.raw["count"], 2);
    }

    #[test]
    fn too_few_rows_fail_the_model_and_report_the_count() {
        let verdict = CapturedRowsCount
            .evaluate(&json!({"name": "items", "min": 2}), &stage(vec![json!({})]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(0))
        );
        assert_eq!(verdict.raw["reason_code"], "below_min");
        assert_eq!(verdict.raw["count"], 1);
    }

    #[test]
    fn too_many_rows_fail_the_model() {
        let verdict = CapturedRowsCount.evaluate(
            &json!({"name": "items", "min": 0, "max": 1}),
            &stage(vec![json!({}), json!({})]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(0))
        );
        assert_eq!(verdict.raw["reason_code"], "above_max");
        assert_eq!(verdict.raw["count"], 2);
    }

    #[test]
    fn a_capture_the_stage_never_produced_is_a_grader_outcome() {
        let verdict = CapturedRowsCount
            .evaluate(&json!({"name": "other", "min": 1}), &stage(vec![json!({})]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Grader, None)
        );
        assert_eq!(verdict.raw["reason_code"], "missing_capture");
        assert_eq!(verdict.raw["count"], Value::Null);
    }

    /// A range no count can satisfy would otherwise fail the subject on every
    /// trial of the case, which is evidence about the case's author.
    #[test]
    fn a_max_below_the_min_is_bad_params_and_never_a_model_failure() {
        for rows in [vec![], vec![json!({})], vec![json!({}), json!({})]] {
            let verdict = CapturedRowsCount.evaluate(
                &json!({"name": "items", "min": 2, "max": 1}),
                &stage(rows.clone()),
            );
            assert_eq!(
                (verdict.kind, verdict.score_bp),
                (OutcomeKind::Grader, None),
                "{rows:?}"
            );
            assert_eq!(verdict.raw["reason_code"], "bad_params");
        }
        // The boundary is still a usable range: exactly `min` rows pass.
        let verdict = CapturedRowsCount.evaluate(
            &json!({"name": "items", "min": 1, "max": 1}),
            &stage(vec![json!({})]),
        );
        assert_eq!(verdict.raw["reason_code"], "in_range");
    }

    #[test]
    fn params_that_do_not_parse_are_a_grader_outcome_carrying_the_error() {
        let verdict = CapturedRowsCount.evaluate(&json!({"min": "two"}), &stage(vec![]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Grader, None)
        );
        assert_eq!(verdict.raw["reason_code"], "bad_params");
        assert_eq!(verdict.raw["count"], Value::Null);
        let detail = verdict.raw["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("two"),
            "the serde error names the bad value: {detail}"
        );
    }
}
