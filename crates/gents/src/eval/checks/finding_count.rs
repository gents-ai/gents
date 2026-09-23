//! `finding_count`: how many findings the rows hold in total.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{all_findings, failed, item_rows, parse_params, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"count": <u64>}`. Passes when the findings of all rows number
/// exactly `count`. Reason codes: `count_matches`, `count_differs`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingCount;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    count: u64,
}

impl Check for FindingCount {
    fn name(&self) -> &'static str {
        "finding_count"
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
    let found = all_findings(rows)?.len() as u64;
    let extra = json!({"rows": rows.len(), "findings": found, "expected": params.count});
    Ok(if found == params.count {
        passed("count_matches", format!("{found} findings"), extra)
    } else {
        failed(
            "count_differs",
            format!("{found} findings, expected {}", params.count),
            extra,
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{
        fail, finding, grader, no_items, outcome, pass, row, stage,
    };

    #[test]
    fn findings_are_counted_across_rows() {
        let rows = vec![
            row("s", vec![finding("c-1", "a", "open", "x")]),
            row("s", vec![finding("c-2", "b", "open", "y")]),
        ];
        assert_eq!(
            outcome(&FindingCount.evaluate(&json!({"count": 2}), &stage(rows.clone()))),
            pass("count_matches")
        );
        let verdict = FindingCount.evaluate(&json!({"count": 1}), &stage(rows));
        assert_eq!(outcome(&verdict), fail("count_differs"));
        assert_eq!(
            (
                verdict.raw["findings"].clone(),
                verdict.raw["expected"].clone()
            ),
            (json!(2), json!(1))
        );
        assert_eq!(
            outcome(&FindingCount.evaluate(&json!({"count": 0}), &stage(vec![]))),
            pass("count_matches")
        );
    }

    #[test]
    fn failures_that_are_not_about_the_count() {
        assert_eq!(
            outcome(
                &FindingCount
                    .evaluate(&json!({"count": 0}), &stage(vec![json!({"payload": null})]))
            ),
            fail("payload_unreadable")
        );
        let verdict = FindingCount.evaluate(&json!({"count": 0}), &stage(vec![json!({})]));
        assert_eq!(outcome(&verdict), grader("missing_field"));
        assert_eq!(verdict.raw["rows"], 1);
        assert_eq!(
            outcome(&FindingCount.evaluate(&json!({"count": -1}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&FindingCount.evaluate(&json!({"count": 0}), &no_items())),
            grader("missing_capture")
        );
    }
}
