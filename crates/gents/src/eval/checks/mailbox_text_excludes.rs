//! `mailbox_text_excludes`: banned words appear nowhere in the mailbox rows.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    contains, failed, field, item_rows, non_empty_keywords, parse_params, parse_payload, passed,
    text_field, tokens, Finding,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"keywords": [<string>, …]}`, at least one. Passes when no keyword
/// occurs (token match) in any row's `title`, `summary` or `payload`. The
/// payload is read through its parsed findings' `condition + " " + detail`,
/// and as raw text only when it does not parse. This is the inherited #1512
/// three-field scope, kept deliberately: a chatty summary fails a ban (see
/// CHECKS.md "Gaps"). Reason codes: `absent`, `keyword_present`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct MailboxTextExcludes;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    keywords: Vec<String>,
}

impl Check for MailboxTextExcludes {
    fn name(&self) -> &'static str {
        "mailbox_text_excludes"
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
    let banned = non_empty_keywords("keywords", &params.keywords)?;
    let rows = item_rows(stage)?;
    let mut hits = Vec::new();
    for index in 0..rows.len() {
        let title = vec![tokens(text_field(rows, index, "title")?)];
        let summary = vec![tokens(text_field(rows, index, "summary")?)];
        let payload = field(rows, index, "payload")?;
        let payload: Vec<Vec<String>> = match parse_payload(payload, None) {
            Ok(found) => found.iter().map(Finding::text_tokens).collect(),
            // Unreadable is still text the subject wrote.
            Err(_) => vec![tokens(payload.as_str().unwrap_or_default())],
        };
        for (name, texts) in [("title", title), ("summary", summary), ("payload", payload)] {
            for (keyword, wanted) in params.keywords.iter().zip(&banned) {
                if texts.iter().any(|text| contains(text, wanted)) {
                    hits.push(json!({"row": index, "field": name, "keyword": keyword}));
                }
            }
        }
    }
    Ok(if hits.is_empty() {
        passed(
            "absent",
            format!("none of {} keywords appears", params.keywords.len()),
            json!({"rows": rows.len()}),
        )
    } else {
        failed(
            "keyword_present",
            format!("{} banned keyword hits", hits.len()),
            json!({"rows": rows.len(), "hits": hits}),
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

    fn rows() -> Vec<Value> {
        vec![row(
            "Disk usage high on archive-02",
            vec![finding(
                "c-1",
                "sync_worker_lagging",
                "open",
                "4419 jobs behind",
            )],
        )]
    }

    #[test]
    fn a_banned_word_absent_from_title_summary_and_findings_passes() {
        let verdict =
            MailboxTextExcludes.evaluate(&json!({"keywords": ["inode", "state"]}), &stage(rows()));
        assert_eq!(
            outcome(&verdict),
            pass("absent"),
            "payload JSON keys are not text: {}",
            verdict.raw
        );
        assert_eq!(verdict.raw["rows"], 1);
    }

    #[test]
    fn a_banned_word_in_the_summary_or_a_finding_fails_and_names_where() {
        let verdict = MailboxTextExcludes.evaluate(
            &json!({"keywords": ["disk", "sync-worker"]}),
            &stage(rows()),
        );
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(
            verdict.raw["hits"],
            json!([
                {"row": 0, "field": "summary", "keyword": "disk"},
                {"row": 0, "field": "payload", "keyword": "sync-worker"},
            ])
        );
    }

    #[test]
    fn an_unreadable_payload_is_still_searched_as_text() {
        let broken =
            json!({"title": "t", "summary": null, "payload": "not json: pcap-export stuck"});
        let verdict = MailboxTextExcludes
            .evaluate(&json!({"keywords": ["pcap-export"]}), &stage(vec![broken]));
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(verdict.raw["hits"][0]["field"], "payload");
    }

    #[test]
    fn grader_outcomes() {
        let no_summary = json!({"title": "t", "payload": null});
        assert_eq!(
            outcome(
                &MailboxTextExcludes
                    .evaluate(&json!({"keywords": ["x"]}), &stage(vec![no_summary]))
            ),
            grader("missing_field")
        );
        assert_eq!(
            outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": []}), &stage(rows()))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": ["%%"]}), &stage(rows()))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": ["x"]}), &no_items())),
            grader("missing_capture")
        );
    }
}
