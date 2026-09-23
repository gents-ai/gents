//! `finding_state_counts`: how many findings are open and how many resolved.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, failed, item_rows, parse_params, passed, FindingState,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"open": <u64>, "resolved": <u64>}`. Passes when the findings of
/// all rows tally exactly so. Reason codes: `counts_match`, `counts_differ`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingStateCounts;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    open: u64,
    resolved: u64,
}

impl Check for FindingStateCounts {
    fn name(&self) -> &'static str {
        "finding_state_counts"
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
    let found = all_findings(rows)?;
    let open = found
        .iter()
        .filter(|finding| finding.state == FindingState::Open)
        .count() as u64;
    let resolved = found.len() as u64 - open;
    let extra = json!({
        "rows": rows.len(),
        "open": open,
        "resolved": resolved,
        "expected_open": params.open,
        "expected_resolved": params.resolved,
    });
    Ok(if (open, resolved) == (params.open, params.resolved) {
        passed(
            "counts_match",
            format!("{open} open, {resolved} resolved"),
            extra,
        )
    } else {
        failed(
            "counts_differ",
            format!(
                "{open} open and {resolved} resolved, expected {} and {}",
                params.open, params.resolved
            ),
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
    fn open_and_resolved_are_tallied_separately() {
        let rows = vec![row(
            "s",
            vec![
                finding("c-1", "a", "open", "x"),
                finding("c-1", "b", "resolved", "y"),
            ],
        )];
        assert_eq!(
            outcome(
                &FindingStateCounts
                    .evaluate(&json!({"open": 1, "resolved": 1}), &stage(rows.clone()))
            ),
            pass("counts_match")
        );
        let verdict = FindingStateCounts.evaluate(&json!({"open": 0, "resolved": 2}), &stage(rows));
        assert_eq!(outcome(&verdict), fail("counts_differ"));
        assert_eq!(
            (verdict.raw["open"].clone(), verdict.raw["resolved"].clone()),
            (json!(1), json!(1))
        );
    }

    #[test]
    fn failures_that_are_not_about_the_tally() {
        assert_eq!(
            outcome(&FindingStateCounts.evaluate(
                &json!({"open": 0, "resolved": 0}),
                &stage(vec![json!({"payload": "x"})])
            )),
            fail("payload_unreadable")
        );
        let verdict = FindingStateCounts
            .evaluate(&json!({"open": 0, "resolved": 0}), &stage(vec![json!({})]));
        assert_eq!(outcome(&verdict), grader("missing_field"));
        assert_eq!(verdict.raw["rows"], 1);
        assert_eq!(
            outcome(&FindingStateCounts.evaluate(&json!({"open": 1}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&FindingStateCounts.evaluate(&json!({"open": 0, "resolved": 0}), &no_items())),
            grader("missing_capture")
        );
    }
}
