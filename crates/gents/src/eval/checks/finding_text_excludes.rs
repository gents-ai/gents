//! `finding_text_excludes`: one correlation's findings avoid banned words.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, contains, failed, item_rows, non_empty_keywords, parse_params, passed,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"correlation": <string>, "keywords": [<string>, …]}`, at least one
/// keyword. Passes when no keyword occurs (token match) in the `condition` or
/// `detail` of any finding with that correlation: the negative
/// cross-correlation claim `mailbox_text_excludes` cannot express. Reason
/// codes: `absent`, `keyword_present`, `payload_unreadable`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct FindingTextExcludes;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    correlation: String,
    keywords: Vec<String>,
}

impl Check for FindingTextExcludes {
    fn name(&self) -> &'static str {
        "finding_text_excludes"
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
    let found = all_findings(rows)?;
    let mut hits = Vec::new();
    for (index, finding) in found
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.correlation == params.correlation)
    {
        let text = finding.text_tokens();
        for (keyword, wanted) in params.keywords.iter().zip(&banned) {
            if contains(&text, wanted) {
                hits.push(
                    json!({"finding": index, "condition": finding.condition, "keyword": keyword}),
                );
            }
        }
    }
    Ok(if hits.is_empty() {
        passed(
            "absent",
            format!("no banned keyword on {}", params.correlation),
            json!({"rows": rows.len()}),
        )
    } else {
        failed(
            "keyword_present",
            format!(
                "{} banned keyword hits on {}",
                hits.len(),
                params.correlation
            ),
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
            "s",
            vec![
                finding("c-13-a", "api_errors", "open", "api-gateway returning 502"),
                finding("c-13-b", "worker_pool_exhausted", "open", "730 jobs queued"),
            ],
        )]
    }

    #[test]
    fn a_ban_binds_only_its_own_correlation() {
        let params = json!({"correlation": "c-13-a", "keywords": ["worker"]});
        assert_eq!(
            outcome(&FindingTextExcludes.evaluate(&params, &stage(rows()))),
            pass("absent")
        );
        let params = json!({"correlation": "c-13-b", "keywords": ["worker-pool", "502"]});
        let verdict = FindingTextExcludes.evaluate(&params, &stage(rows()));
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(
            verdict.raw["hits"],
            json!([{"finding": 1, "condition": "worker_pool_exhausted", "keyword": "worker-pool"}])
        );
    }

    #[test]
    fn failures_that_are_not_about_the_ban() {
        let params = json!({"correlation": "c", "keywords": ["x"]});
        assert_eq!(
            outcome(&FindingTextExcludes.evaluate(&params, &stage(vec![json!({"payload": 3})]))),
            fail("payload_unreadable")
        );
        assert_eq!(
            outcome(&FindingTextExcludes.evaluate(&params, &stage(vec![json!({})]))),
            grader("missing_field")
        );
        assert_eq!(
            outcome(
                &FindingTextExcludes
                    .evaluate(&json!({"correlation": "c", "keywords": []}), &stage(vec![]))
            ),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&FindingTextExcludes.evaluate(&json!({"keywords": ["x"]}), &stage(vec![]))),
            grader("bad_params")
        );
        assert_eq!(
            outcome(&FindingTextExcludes.evaluate(&params, &no_items())),
            grader("missing_capture")
        );
    }
}
