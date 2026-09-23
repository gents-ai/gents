//! `finding_pairs_unique`: one finding per (correlation, condition).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, failed, item_rows, parse_params, passed, NoParams,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{}`. Passes when no two findings, across all rows, share
/// `(correlation, condition)`. Reason codes: `unique`, `duplicate_pair`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingPairsUnique;

impl Check for FindingPairsUnique {
    fn name(&self) -> &'static str {
        "finding_pairs_unique"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let NoParams {} = parse_params(params)?;
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let mut seen = BTreeSet::new();
    for finding in &found {
        if !seen.insert((finding.correlation.as_str(), finding.condition.as_str())) {
            return Ok(failed(
                "duplicate_pair",
                format!(
                    "({}, {}) appears more than once",
                    finding.correlation, finding.condition
                ),
                json!({"rows": rows.len(), "correlation": finding.correlation, "condition": finding.condition}),
            ));
        }
    }
    Ok(passed(
        "unique",
        format!("{} findings, each pair once", found.len()),
        json!({"rows": rows.len(), "findings": found.len()}),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{
        fail, finding, grader, no_items, outcome, pass, row, stage,
    };

    #[test]
    fn a_pair_seen_twice_across_rows_fails_and_distinct_pairs_pass() {
        let distinct = vec![row(
            "s",
            vec![
                finding("c-1", "fan", "open", "x"),
                finding("c-2", "fan", "open", "x"),
            ],
        )];
        assert_eq!(
            outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(distinct))),
            pass("unique")
        );
        let twice = vec![
            row("s", vec![finding("c-1", "fan", "open", "x")]),
            row("s", vec![finding("c-1", "fan", "resolved", "y")]),
        ];
        let verdict = FindingPairsUnique.evaluate(&Value::Null, &stage(twice));
        assert_eq!(outcome(&verdict), fail("duplicate_pair"));
        assert_eq!(
            (
                verdict.raw["correlation"].clone(),
                verdict.raw["condition"].clone()
            ),
            (json!("c-1"), json!("fan"))
        );
    }

    #[test]
    fn unreadable_payloads_fail_and_graders_stay_graders() {
        let broken = vec![json!({"payload": "nope"})];
        assert_eq!(
            outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(broken))),
            fail("payload_unreadable")
        );
        assert_eq!(
            outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(vec![json!({})]))),
            grader("missing_field")
        );
        assert_eq!(
            outcome(&FindingPairsUnique.evaluate(&json!({"x": 1}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&FindingPairsUnique.evaluate(&json!({}), &no_items())),
            grader("missing_capture")
        );
    }
}
