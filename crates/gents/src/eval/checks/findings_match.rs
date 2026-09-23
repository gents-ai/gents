//! `findings_match`: the findings the case expects are there, each once.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, contains, failed, item_rows, non_empty_keywords, parse_params, passed, Finding,
    FindingState,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"matchers": [{"correlation", "state", "keywords_all": [..]}],
/// "exact": <bool, default false>}`. A matcher is satisfied by a finding with
/// its correlation and state whose `condition + " " + detail` contains every
/// keyword (token match). Passes when a one-to-one assignment satisfies every
/// matcher and, with `exact`, leaves no finding over. Reason codes: `matched`,
/// `unmatched_matcher`, `unexpected_finding`, `payload_unreadable`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct FindingsMatch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matcher {
    correlation: String,
    state: FindingState,
    keywords_all: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    matchers: Vec<Matcher>,
    #[serde(default)]
    exact: bool,
}

impl Check for FindingsMatch {
    fn name(&self) -> &'static str {
        "findings_match"
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
    let mut wanted = Vec::with_capacity(params.matchers.len());
    for (index, matcher) in params.matchers.iter().enumerate() {
        wanted.push(non_empty_keywords(
            &format!("matcher {index}"),
            &matcher.keywords_all,
        )?);
    }
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let texts: Vec<Vec<String>> = found.iter().map(Finding::text_tokens).collect();
    let candidates: Vec<Vec<usize>> = params
        .matchers
        .iter()
        .zip(&wanted)
        .map(|(matcher, keywords)| {
            same_correlation_and_state(&found, matcher)
                .filter(|index| {
                    keywords
                        .iter()
                        .all(|keyword| contains(&texts[*index], keyword))
                })
                .collect()
        })
        .collect();
    let assigned = assign(&candidates, found.len());
    let mut taken_by: Vec<Option<usize>> = vec![None; found.len()];
    for (matcher, finding) in assigned.iter().enumerate() {
        if let Some(finding) = finding {
            taken_by[*finding] = Some(matcher);
        }
    }

    let unmatched: Vec<Value> = assigned
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.is_none())
        .map(|(index, _)| {
            let matcher = &params.matchers[index];
            let near: Vec<Value> = same_correlation_and_state(&found, matcher)
                .map(|finding| {
                    let missing: Vec<&String> = matcher
                        .keywords_all
                        .iter()
                        .zip(&wanted[index])
                        .filter(|(_, keyword)| !contains(&texts[finding], keyword))
                        .map(|(keyword, _)| keyword)
                        .collect();
                    let mut entry = json!({"finding": finding, "missing_keywords": missing});
                    // A near finding lacking no keyword was taken by another
                    // matcher; name it so the entry explains itself.
                    if let Some(other) = taken_by[finding] {
                        entry["assigned_to"] = json!(other);
                    }
                    entry
                })
                .collect();
            json!({
                "matcher": index,
                "correlation": matcher.correlation,
                "state": matcher.state,
                "keywords_all": matcher.keywords_all,
                "near": near,
            })
        })
        .collect();
    if !unmatched.is_empty() {
        return Ok(failed(
            "unmatched_matcher",
            format!(
                "{} of {} matchers found no finding",
                unmatched.len(),
                params.matchers.len()
            ),
            json!({"rows": rows.len(), "unmatched": unmatched, "findings": found}),
        ));
    }
    if params.exact {
        let used: BTreeSet<usize> = assigned.iter().flatten().copied().collect();
        let unexpected: Vec<&Finding> = found
            .iter()
            .enumerate()
            .filter(|(index, _)| !used.contains(index))
            .map(|(_, finding)| finding)
            .collect();
        if !unexpected.is_empty() {
            return Ok(failed(
                "unexpected_finding",
                format!("{} findings match no matcher", unexpected.len()),
                json!({"rows": rows.len(), "unexpected": unexpected, "findings": found}),
            ));
        }
    }
    Ok(passed(
        "matched",
        format!(
            "{} matchers matched among {} findings",
            params.matchers.len(),
            found.len()
        ),
        json!({"rows": rows.len(), "findings": found.len()}),
    ))
}

/// The indices of the findings a matcher could be about: its correlation and
/// state, whatever their text.
fn same_correlation_and_state<'a>(
    found: &'a [Finding],
    matcher: &'a Matcher,
) -> impl Iterator<Item = usize> + 'a {
    found
        .iter()
        .enumerate()
        .filter(move |(_, finding)| {
            finding.correlation == matcher.correlation && finding.state == matcher.state
        })
        .map(|(index, _)| index)
}

/// A maximum one-to-one assignment of matchers to findings (Kuhn's augmenting
/// paths), deterministic in matcher and finding order. `candidates[m]` lists
/// the findings matcher `m` accepts; the result gives each matcher's finding.
fn assign(candidates: &[Vec<usize>], findings: usize) -> Vec<Option<usize>> {
    let mut owner: Vec<Option<usize>> = vec![None; findings];
    for matcher in 0..candidates.len() {
        let mut seen = vec![false; findings];
        augment(matcher, candidates, &mut owner, &mut seen);
    }
    let mut assigned = vec![None; candidates.len()];
    for (finding, matcher) in owner.iter().enumerate() {
        if let Some(matcher) = matcher {
            assigned[*matcher] = Some(finding);
        }
    }
    assigned
}

fn augment(
    matcher: usize,
    candidates: &[Vec<usize>],
    owner: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for &finding in &candidates[matcher] {
        if seen[finding] {
            continue;
        }
        seen[finding] = true;
        let current = owner[finding];
        if current.is_none_or(|other| augment(other, candidates, owner, seen)) {
            owner[finding] = Some(matcher);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{
        fail, finding, grader, no_items, outcome, pass, row, stage,
    };

    fn matcher(correlation: &str, state: &str, keywords: &[&str]) -> Value {
        json!({"correlation": correlation, "state": state, "keywords_all": keywords})
    }

    #[test]
    fn every_matcher_finds_its_own_finding_by_tokens() {
        let rows = vec![row(
            "s",
            vec![
                finding(
                    "c-01-a",
                    "disk_usage_high",
                    "open",
                    "/var at 88% and rising",
                ),
                finding(
                    "c-18-a",
                    "cert_renewer_stopped",
                    "open",
                    "cert-renewer is stopped",
                ),
            ],
        )];
        let params = json!({"matchers": [
            matcher("c-01-a", "open", &["disk", "88"]),
            matcher("c-18-a", "open", &["cert", "stopped"]),
        ], "exact": true});
        let verdict = FindingsMatch.evaluate(&params, &stage(rows));
        assert_eq!(outcome(&verdict), pass("matched"), "{}", verdict.raw);
    }

    #[test]
    fn the_assignment_is_one_to_one_and_finds_it_when_a_greedy_pass_would_not() {
        // m0 fits both findings, m1 only the first: greedy m0 -> f0 would
        // strand m1; the augmenting path moves m0 to f1.
        let rows = vec![row(
            "s",
            vec![
                finding("c-1", "fan_stalled", "open", "fan 2 stalled at 0 rpm"),
                finding("c-1", "fan_stalled_psu", "open", "fan stalled"),
            ],
        )];
        let params = json!({"matchers": [
            matcher("c-1", "open", &["fan", "stalled"]),
            matcher("c-1", "open", &["fan", "rpm"]),
        ], "exact": true});
        assert_eq!(
            outcome(&FindingsMatch.evaluate(&params, &stage(rows))),
            pass("matched")
        );

        let one = vec![row(
            "s",
            vec![finding("c-1", "fan_stalled", "open", "stalled")],
        )];
        let twice = json!({"matchers": [matcher("c-1", "open", &["fan"]), matcher("c-1", "open", &["fan"])]});
        let verdict = FindingsMatch.evaluate(&twice, &stage(one));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(verdict.raw["unmatched"][0]["matcher"], 1);
        assert_eq!(
            verdict.raw["unmatched"][0]["near"],
            json!([{"finding": 0, "missing_keywords": [], "assigned_to": 0}]),
            "the near finding fits, but matcher 0 took it"
        );
    }

    #[test]
    fn an_unmatched_matcher_reports_the_keywords_its_near_findings_lack() {
        let rows = vec![row(
            "s",
            vec![finding(
                "c-01-a",
                "disk_usage_high",
                "open",
                "/var nearly full",
            )],
        )];
        let params = json!({"matchers": [matcher("c-01-a", "open", &["disk", "88"])]});
        let verdict = FindingsMatch.evaluate(&params, &stage(rows));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(
            verdict.raw["unmatched"][0]["near"],
            json!([{"finding": 0, "missing_keywords": ["88"]}])
        );
        assert_eq!(verdict.raw["findings"][0]["condition"], "disk_usage_high");

        let wrong_state = vec![row(
            "s",
            vec![finding(
                "c-09-a",
                "replication_lag",
                "open",
                "lag caught up",
            )],
        )];
        let params = json!({"matchers": [matcher("c-09-a", "resolved", &["replication", "lag"])]});
        let verdict = FindingsMatch.evaluate(&params, &stage(wrong_state));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(verdict.raw["unmatched"][0]["near"], json!([]));
    }

    #[test]
    fn exact_refuses_a_leftover_finding_and_inexact_allows_it() {
        let rows = vec![row(
            "s",
            vec![
                finding("c-1", "disk_usage_high", "open", "88%"),
                finding("c-1", "inode_usage_high", "open", "inodes"),
            ],
        )];
        let one = json!([matcher("c-1", "open", &["disk"])]);
        let verdict = FindingsMatch.evaluate(
            &json!({"matchers": one, "exact": true}),
            &stage(rows.clone()),
        );
        assert_eq!(outcome(&verdict), fail("unexpected_finding"));
        assert_eq!(
            verdict.raw["unexpected"][0]["condition"],
            "inode_usage_high"
        );
        assert_eq!(
            outcome(&FindingsMatch.evaluate(&json!({"matchers": one}), &stage(rows))),
            pass("matched")
        );
        assert_eq!(
            outcome(
                &FindingsMatch.evaluate(&json!({"matchers": [], "exact": true}), &stage(vec![]))
            ),
            pass("matched"),
            "no row and no matcher is the zero-condition expectation"
        );
    }

    #[test]
    fn bad_params_unreadable_payloads_and_missing_captures() {
        for params in [
            json!({"matchers": [matcher("c", "closed", &["x"])]}),
            json!({"matchers": [matcher("c", "open", &[])]}),
            json!({"matchers": [matcher("c", "open", &["--"])]}),
            json!({"matchers": [], "extra": 1}),
        ] {
            assert_eq!(
                outcome(&FindingsMatch.evaluate(&params, &stage(vec![]))),
                grader("bad_params"),
                "{params}"
            );
        }
        let params = json!({"matchers": [matcher("c", "open", &["x"])]});
        assert_eq!(
            outcome(&FindingsMatch.evaluate(&params, &stage(vec![json!({"payload": "{"})]))),
            fail("payload_unreadable")
        );
        let verdict = FindingsMatch.evaluate(&params, &stage(vec![json!({})]));
        assert_eq!(outcome(&verdict), grader("missing_field"));
        assert_eq!(verdict.raw["rows"], 1, "the capture was read");
        assert_eq!(
            outcome(&FindingsMatch.evaluate(&params, &no_items())),
            grader("missing_capture")
        );
    }
}
