//! `mailbox_open_row`: the subject keeps exactly one open record.

use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, field, item_rows, parse_params, passed, NoParams};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{}`. Passes when the `items` capture holds exactly one row and its
/// `status` is `"open"`. Reason codes: `one_open_row`, `no_rows`,
/// `several_rows`, `not_open`; grader: `bad_params`, `missing_capture`,
/// `missing_field` (the capture did not select `status`).
pub struct MailboxOpenRow;

impl Check for MailboxOpenRow {
    fn name(&self) -> &'static str {
        "mailbox_open_row"
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
    Ok(match rows {
        [] => failed(
            "no_rows",
            "the stage left no mailbox row".into(),
            json!({"rows": 0}),
        ),
        [_] => {
            let status = field(rows, 0, "status")?;
            let extra = json!({"rows": 1, "status": status});
            if status.as_str() == Some("open") {
                passed("one_open_row", "one row, open".into(), extra)
            } else {
                failed(
                    "not_open",
                    format!("the one row has status {status}"),
                    extra,
                )
            }
        }
        _ => failed(
            "several_rows",
            format!("{} rows, expected one open row", rows.len()),
            json!({"rows": rows.len()}),
        ),
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
    fn exactly_one_open_row_passes_and_everything_else_says_why_not() {
        let open = row("s", vec![]);
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![open.clone()]))),
            pass("one_open_row")
        );
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&Value::Null, &stage(vec![open.clone()]))),
            pass("one_open_row")
        );
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![]))),
            fail("no_rows")
        );
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![open.clone(), open.clone()]))),
            fail("several_rows")
        );
        let mut dismissed = open;
        dismissed["status"] = json!("dismissed");
        let verdict = MailboxOpenRow.evaluate(&json!({}), &stage(vec![dismissed]));
        assert_eq!(outcome(&verdict), fail("not_open"));
        assert_eq!(verdict.raw["status"], "dismissed");
    }

    #[test]
    fn a_row_without_status_bad_params_and_no_capture_are_grader_outcomes() {
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![json!({"title": "t"})]))),
            grader("missing_field")
        );
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({"count": 1}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&MailboxOpenRow.evaluate(&json!({}), &no_items())),
            grader("missing_capture")
        );
    }
}
