//! `payload_well_formed`: every row's payload is the subject's output contract.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, field, item_rows, parse_params, parse_payload, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"version": <u64>}`. Passes when every row's `payload` is JSON text
/// of `{"version": <version>, "findings": [{correlation, condition, state,
/// detail}]}` with `state` in `{open, resolved}`; no rows pass vacuously.
/// Reason codes: `well_formed`, `payload_missing`, `payload_not_string`,
/// `payload_not_json`, `payload_bad_shape`, `payload_wrong_version`,
/// `payload_bad_state`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct PayloadWellFormed;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    version: u64,
}

impl Check for PayloadWellFormed {
    fn name(&self) -> &'static str {
        "payload_well_formed"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let rows = item_rows(stage)?;
    let mut findings = 0usize;
    for index in 0..rows.len() {
        match parse_payload(field(rows, index, "payload")?, Some(params.version)) {
            Ok(found) => findings += found.len(),
            Err(defect) => {
                return Ok(failed(
                    defect.reason_code(),
                    format!("row {index}: {}", defect.reason_code()),
                    json!({"rows": rows.len(), "row": index}),
                ))
            }
        }
    }
    Ok(passed(
        "well_formed",
        format!("{} rows, {findings} findings", rows.len()),
        json!({"rows": rows.len(), "findings": findings}),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{
        fail, finding, grader, no_items, outcome, pass, row, stage,
    };

    fn with_payload(payload: Value) -> Value {
        json!({"title": "t", "summary": "s", "status": "open", "payload": payload})
    }

    #[test]
    fn every_row_in_the_contract_shape_passes_and_no_rows_pass_vacuously() {
        let rows = vec![row(
            "s",
            vec![
                finding("c-1", "a", "open", "x"),
                finding("c-1", "b", "resolved", "y"),
            ],
        )];
        let verdict = PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(rows));
        assert_eq!(outcome(&verdict), pass("well_formed"));
        assert_eq!(
            (verdict.raw["rows"].clone(), verdict.raw["findings"].clone()),
            (json!(1), json!(2))
        );
        assert_eq!(
            outcome(&PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(vec![]))),
            pass("well_formed")
        );
    }

    #[test]
    fn each_defect_fails_with_its_own_code_and_names_the_row() {
        let cases = [
            (json!(null), "payload_missing"),
            (json!({"version": 1, "findings": []}), "payload_not_string"),
            (json!("{"), "payload_not_json"),
            (json!(r#"{"version": 1}"#), "payload_bad_shape"),
            (
                json!(r#"{"version": 2, "findings": []}"#),
                "payload_wrong_version",
            ),
            (
                json!(
                    r#"{"version": 1, "findings": [{"correlation": "c", "condition": "x", "state": "closed", "detail": "d"}]}"#
                ),
                "payload_bad_state",
            ),
        ];
        for (payload, code) in cases {
            let rows = vec![row("fine", vec![]), with_payload(payload.clone())];
            let verdict = PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(rows));
            assert_eq!(outcome(&verdict), fail(code), "{payload}");
            assert_eq!(verdict.raw["row"], 1);
        }
    }

    #[test]
    fn grader_outcomes() {
        assert_eq!(
            outcome(
                &PayloadWellFormed
                    .evaluate(&json!({"version": 1}), &stage(vec![json!({"title": "t"})]))
            ),
            grader("missing_field")
        );
        assert_eq!(
            outcome(&PayloadWellFormed.evaluate(&json!({"version": "1"}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&PayloadWellFormed.evaluate(&json!({"version": 1}), &no_items())),
            grader("missing_capture")
        );
    }
}
