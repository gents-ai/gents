//! `mailbox_item_count`: how many mailbox rows the stage left.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, item_rows, parse_params, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"count": <u64>}`. Passes when the `items` capture holds exactly
/// `count` rows. Reason codes: `count_matches`, `count_differs`; grader:
/// `bad_params`, `missing_capture`.
pub struct MailboxItemCount;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    count: u64,
}

impl Check for MailboxItemCount {
    fn name(&self) -> &'static str {
        "mailbox_item_count"
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
    let found = item_rows(stage)?.len() as u64;
    let extra = json!({"rows": found, "expected": params.count});
    Ok(if found == params.count {
        passed("count_matches", format!("{found} rows"), extra)
    } else {
        failed(
            "count_differs",
            format!("{found} rows, expected {}", params.count),
            extra,
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{
        fail, grader, no_items, outcome, pass, row, stage,
    };

    #[test]
    fn the_row_count_decides() {
        let one = stage(vec![row("s", vec![])]);
        assert_eq!(
            outcome(&MailboxItemCount.evaluate(&json!({"count": 1}), &one)),
            pass("count_matches")
        );
        assert_eq!(
            outcome(&MailboxItemCount.evaluate(&json!({"count": 0}), &stage(vec![]))),
            pass("count_matches")
        );
        let verdict = MailboxItemCount.evaluate(&json!({"count": 0}), &one);
        assert_eq!(outcome(&verdict), fail("count_differs"));
        assert_eq!(
            (verdict.raw["rows"].clone(), verdict.raw["expected"].clone()),
            (json!(1), json!(0))
        );
    }

    #[test]
    fn bad_params_and_a_missing_capture_are_grader_outcomes() {
        let one = stage(vec![row("s", vec![])]);
        assert_eq!(
            outcome(&MailboxItemCount.evaluate(&json!({"count": "one"}), &one)),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&MailboxItemCount.evaluate(&json!({"count": 1}), &no_items())),
            grader("missing_capture")
        );
        assert_eq!(
            (MailboxItemCount.name(), MailboxItemCount.version()),
            ("mailbox_item_count", "1")
        );
    }
}
