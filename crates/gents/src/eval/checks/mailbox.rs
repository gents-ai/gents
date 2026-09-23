//! Shared predicates for the checks over the monitor's `items` capture.
//!
//! Every mailbox check reads the `MailboxItem` rows a stage captured under
//! [`ITEMS`]. The rules they share live here once: how a row's `payload`
//! parses into findings, how a keyword matches text, how params and fields
//! are read, and how a verdict is shaped. `packs/eval_monitor/CHECKS.md` is
//! the inventory these rules come from.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::eval::checks::CheckVerdict;
use crate::eval::runner::executor::{CaptureResult, StageEvidence};
use crate::eval::OutcomeKind;

/// The capture every mailbox check reads.
pub(crate) const ITEMS: &str = "items";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingState {
    Open,
    Resolved,
}

/// One entry of a row's payload, as the subject wrote it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Finding {
    pub(crate) correlation: String,
    pub(crate) condition: String,
    pub(crate) state: FindingState,
    pub(crate) detail: String,
}

impl Finding {
    /// The matchable text, `condition + " " + detail`, as tokens.
    pub(crate) fn text_tokens(&self) -> Vec<String> {
        tokens(&format!("{} {}", self.condition, self.detail))
    }
}

/// Why a payload is not the subject's output contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PayloadDefect {
    Missing,
    NotString,
    NotJson,
    BadShape,
    WrongVersion,
    BadState,
}

impl PayloadDefect {
    pub(crate) fn reason_code(self) -> &'static str {
        match self {
            Self::Missing => "payload_missing",
            Self::NotString => "payload_not_string",
            Self::NotJson => "payload_not_json",
            Self::BadShape => "payload_bad_shape",
            Self::WrongVersion => "payload_wrong_version",
            Self::BadState => "payload_bad_state",
        }
    }
}

/// A row's `payload`: JSON text of `{"version", "findings": [{correlation,
/// condition, state, detail}]}`, `state` in `{open, resolved}`. Extra keys are
/// tolerated. `version` is compared only when given.
pub(crate) fn parse_payload(
    payload: &Value,
    version: Option<u64>,
) -> Result<Vec<Finding>, PayloadDefect> {
    let text = match payload {
        Value::Null => return Err(PayloadDefect::Missing),
        Value::String(text) => text,
        _ => return Err(PayloadDefect::NotString),
    };
    let parsed: Value = serde_json::from_str(text).map_err(|_| PayloadDefect::NotJson)?;
    let object = parsed.as_object().ok_or(PayloadDefect::BadShape)?;
    let found = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(PayloadDefect::BadShape)?;
    if version.is_some_and(|expected| expected != found) {
        return Err(PayloadDefect::WrongVersion);
    }
    object
        .get("findings")
        .and_then(Value::as_array)
        .ok_or(PayloadDefect::BadShape)?
        .iter()
        .map(finding)
        .collect()
}

fn finding(entry: &Value) -> Result<Finding, PayloadDefect> {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(PayloadDefect::BadShape)
    };
    let state = match text("state")?.as_str() {
        "open" => FindingState::Open,
        "resolved" => FindingState::Resolved,
        _ => return Err(PayloadDefect::BadState),
    };
    Ok(Finding {
        correlation: text("correlation")?,
        condition: text("condition")?,
        state,
        detail: text("detail")?,
    })
}

/// The rows of the `items` capture. A stage without one is a grader outcome:
/// no evidence was read, which says nothing about the subject.
pub(crate) fn item_rows(stage: &StageEvidence) -> Result<&[Value], CheckVerdict> {
    match stage.captures.get(ITEMS) {
        Some(CaptureResult::Documents { rows }) => Ok(rows),
        Some(CaptureResult::Files { .. }) => Err(grader(
            "missing_capture",
            format!("capture {ITEMS} holds files, not documents"),
        )),
        None => Err(grader(
            "missing_capture",
            format!("the stage produced no capture named {ITEMS}"),
        )),
    }
}

/// A field of `rows[index]`. Absent means the capture did not select it — the
/// definition's fault — while `null` is present: the subject left it unset.
/// The capture was read, so the grader verdict still reports its `rows`.
pub(crate) fn field<'a>(
    rows: &'a [Value],
    index: usize,
    name: &str,
) -> Result<&'a Value, CheckVerdict> {
    rows.get(index)
        .and_then(|row| row.get(name))
        .ok_or_else(|| {
            grader_with(
                "missing_field",
                format!("row {index} has no {name} field; the capture must select it"),
                json!({"rows": rows.len()}),
            )
        })
}

/// A text field of `rows[index]`; `null` and non-strings read as empty text.
pub(crate) fn text_field<'a>(
    rows: &'a [Value],
    index: usize,
    name: &str,
) -> Result<&'a str, CheckVerdict> {
    Ok(field(rows, index, name)?.as_str().unwrap_or_default())
}

/// Every finding of every row, in row order. A payload that is not the output
/// contract fails the claim of the check that asked (CHECKS.md, "Payload
/// parse"); a row without a `payload` field is the capture's fault.
pub(crate) fn all_findings(rows: &[Value]) -> Result<Vec<Finding>, CheckVerdict> {
    let mut all = Vec::new();
    for index in 0..rows.len() {
        match parse_payload(field(rows, index, "payload")?, None) {
            Ok(found) => all.extend(found),
            Err(defect) => {
                return Err(failed(
                    "payload_unreadable",
                    format!("row {index}: {}", defect.reason_code()),
                    json!({"rows": rows.len(), "row": index, "defect": defect.reason_code()}),
                ))
            }
        }
    }
    Ok(all)
}

/// A check ref's params; an absent `params` reads as `{}`.
pub(crate) fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, CheckVerdict> {
    let value = if params.is_null() {
        Value::Object(Map::new())
    } else {
        params.clone()
    };
    serde_json::from_value(value).map_err(|error| grader("bad_params", error.to_string()))
}

/// The params of a check that takes none.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoParams {}

/// Lowercase runs of alphanumeric characters; everything else separates.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Each keyword's tokens. A keyword with none could match nothing, or
/// everything, so it is a mistake in the case.
pub(crate) fn keyword_tokens(keywords: &[String]) -> Result<Vec<Vec<String>>, CheckVerdict> {
    keywords
        .iter()
        .map(|keyword| {
            let found = tokens(keyword);
            if found.is_empty() {
                Err(grader(
                    "bad_params",
                    format!("keyword {keyword:?} has no letters or digits"),
                ))
            } else {
                Ok(found)
            }
        })
        .collect()
}

/// The tokens of a keyword list a check requires to name at least one
/// keyword. `what` names the list in the detail (`keywords`, `matcher 2`). An
/// empty list would make a ban vacuous and a matcher match anything, so it is
/// a mistake in the case, like a keyword without letters or digits.
pub(crate) fn non_empty_keywords(
    what: &str,
    keywords: &[String],
) -> Result<Vec<Vec<String>>, CheckVerdict> {
    if keywords.is_empty() {
        return Err(grader("bad_params", format!("{what} names no keyword")));
    }
    keyword_tokens(keywords)
}

/// Whether `keyword`'s tokens occur consecutively in `text`'s tokens.
pub(crate) fn contains(text: &[String], keyword: &[String]) -> bool {
    !keyword.is_empty() && text.windows(keyword.len()).any(|window| window == keyword)
}

pub(crate) fn passed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Passed,
        score_bp: Some(10_000),
        raw: raw(reason_code, detail, extra),
        feedback: None,
    }
}

/// The subject did the wrong thing.
pub(crate) fn failed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::ModelAcceptance,
        score_bp: Some(0),
        raw: raw(reason_code, detail, extra),
        feedback: None,
    }
}

/// The check could not reach a verdict: no evidence about the subject.
pub(crate) fn grader(reason_code: &str, detail: String) -> CheckVerdict {
    grader_with(reason_code, detail, Value::Null)
}

fn grader_with(reason_code: &str, detail: String, extra: Value) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Grader,
        score_bp: None,
        raw: raw(reason_code, detail, extra),
        feedback: None,
    }
}

fn raw(reason_code: &str, detail: String, extra: Value) -> Value {
    let mut object = Map::new();
    object.insert("reason_code".into(), reason_code.into());
    object.insert("detail".into(), detail.into());
    if let Value::Object(extra) = extra {
        object.extend(extra);
    }
    Value::Object(object)
}

#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{json, Value};

    use crate::eval::checks::CheckVerdict;
    use crate::eval::runner::executor::StageEvidence;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;

    /// One completed stage whose `items` capture holds `rows`.
    pub(crate) fn stage(rows: Vec<Value>) -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", super::ITEMS, rows);
        evidence.stages.remove(0)
    }

    /// One completed stage that captured something, but not `items`.
    pub(crate) fn no_items() -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", "other", Vec::new());
        evidence.stages.remove(0)
    }

    pub(crate) fn finding(correlation: &str, condition: &str, state: &str, detail: &str) -> Value {
        json!({"correlation": correlation, "condition": condition, "state": state, "detail": detail})
    }

    /// An open mailbox row whose payload is the JSON text of `findings`.
    pub(crate) fn row(summary: &str, findings: Vec<Value>) -> Value {
        json!({
            "_docID": "bae-row",
            "title": "Monitor findings",
            "summary": summary,
            "status": "open",
            "payload": json!({"version": 1, "findings": findings}).to_string(),
        })
    }

    pub(crate) fn outcome(verdict: &CheckVerdict) -> (OutcomeKind, Option<u32>, String) {
        (
            verdict.kind,
            verdict.score_bp,
            verdict.raw["reason_code"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    pub(crate) fn pass(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::Passed, Some(10_000), reason.to_owned())
    }

    pub(crate) fn fail(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::ModelAcceptance, Some(0), reason.to_owned())
    }

    pub(crate) fn grader(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::Grader, None, reason.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::test_support::{self, finding, no_items, outcome, row, stage};
    use super::*;

    fn keyword(text: &str) -> Vec<String> {
        tokens(text)
    }

    #[test]
    fn tokens_are_lowercase_alphanumeric_runs() {
        assert_eq!(
            tokens("Disk_usage_HIGH: /var at 88%, api-gateway"),
            ["disk", "usage", "high", "var", "at", "88", "api", "gateway"]
        );
        assert!(tokens(" -%_/ ").is_empty());
    }

    #[test]
    fn a_keyword_matches_a_consecutive_run_of_tokens_never_a_substring() {
        let text = tokens("sync_worker lag at 4419 on api-gateway, disks full");
        assert!(
            contains(&text, &keyword("sync-worker")),
            "hyphen and underscore agree"
        );
        assert!(
            contains(&text, &keyword("api")),
            "the stem of a hyphenated token"
        );
        assert!(contains(&text, &keyword("4419")));
        assert!(
            !contains(&text, &keyword("disk")),
            "disk is not a token of disks"
        );
        assert!(!contains(&text, &keyword("worker-sync")), "order matters");
        assert!(!contains(&text, &[]), "an empty keyword matches nothing");
    }

    #[test]
    fn keywords_without_letters_or_digits_are_bad_params() {
        assert_eq!(
            keyword_tokens(&["disk".into(), "sync-worker".into()]).unwrap(),
            vec![
                vec!["disk".to_owned()],
                vec!["sync".to_owned(), "worker".to_owned()]
            ]
        );
        let verdict = keyword_tokens(&["--".into()]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
    }

    #[test]
    fn an_empty_keyword_list_is_bad_params_and_a_named_one_is_tokenized() {
        let verdict = non_empty_keywords("keywords", &[]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
        assert_eq!(verdict.raw["detail"], "keywords names no keyword");
        assert_eq!(
            non_empty_keywords("matcher 0", &["api-gateway".into()]).unwrap(),
            vec![vec!["api".to_owned(), "gateway".to_owned()]]
        );
        let verdict = non_empty_keywords("matcher 0", &["%%".into()]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
    }

    #[test]
    fn a_payload_parses_into_findings_and_every_defect_has_its_code() {
        let good = json!({"version": 1, "findings": [
            finding("c-1", "disk_usage_high", "open", "88%"),
            finding("c-1", "fan_stalled", "resolved", "spinning again"),
        ]})
        .to_string();
        let found = parse_payload(&json!(good), Some(1)).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].state, FindingState::Resolved);
        assert_eq!(found[0].text_tokens(), ["disk", "usage", "high", "88"]);
        assert!(parse_payload(&json!(good), None).is_ok());

        let cases = [
            (json!(null), PayloadDefect::Missing, "payload_missing"),
            (
                json!({"version": 1}),
                PayloadDefect::NotString,
                "payload_not_string",
            ),
            (json!("{"), PayloadDefect::NotJson, "payload_not_json"),
            (json!("[]"), PayloadDefect::BadShape, "payload_bad_shape"),
            (
                json!(r#"{"version": 1}"#),
                PayloadDefect::BadShape,
                "payload_bad_shape",
            ),
            (
                json!(r#"{"version": 1, "findings": [{"state": "open"}]}"#),
                PayloadDefect::BadShape,
                "payload_bad_shape",
            ),
            (
                json!(r#"{"version": 2, "findings": []}"#),
                PayloadDefect::WrongVersion,
                "payload_wrong_version",
            ),
            (
                json!(
                    r#"{"version": 1, "findings": [{"correlation": "c", "condition": "x", "state": "closed", "detail": "d"}]}"#
                ),
                PayloadDefect::BadState,
                "payload_bad_state",
            ),
        ];
        for (payload, defect, code) in cases {
            assert_eq!(
                parse_payload(&payload, Some(1)).unwrap_err(),
                defect,
                "{payload}"
            );
            assert_eq!(defect.reason_code(), code);
        }
        assert!(
            parse_payload(&json!(r#"{"version": 2, "findings": []}"#), None).is_ok(),
            "without a version the shape alone is checked"
        );
    }

    #[test]
    fn rows_come_from_the_items_capture_or_the_check_is_a_grader_outcome() {
        assert_eq!(item_rows(&stage(vec![json!({})])).unwrap().len(), 1);
        let verdict = item_rows(&no_items()).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_capture"));
    }

    #[test]
    fn a_field_the_capture_did_not_select_is_missing_but_a_null_one_is_present() {
        let rows = [json!({}), json!({}), json!({}), json!({"summary": null})];
        assert_eq!(text_field(&rows, 3, "summary").unwrap(), "");
        let verdict = field(&rows, 3, "title").unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_field"));
        assert!(verdict.raw["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("row 3"));
        assert_eq!(verdict.raw["rows"], 4, "the capture was read");
    }

    #[test]
    fn all_findings_flattens_rows_and_an_unreadable_payload_fails_the_subject() {
        let rows = vec![
            row("a", vec![finding("c-1", "a", "open", "x")]),
            row("b", vec![finding("c-2", "b", "open", "y")]),
        ];
        assert_eq!(all_findings(&rows).unwrap().len(), 2);
        let broken = vec![rows[0].clone(), json!({"payload": "not json"})];
        let verdict = all_findings(&broken).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::fail("payload_unreadable"));
        assert_eq!(verdict.raw["row"], 1);
        assert_eq!(verdict.raw["defect"], "payload_not_json");
        let verdict = all_findings(&[json!({})]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_field"));
        assert_eq!(verdict.raw["rows"], 1);
    }

    #[test]
    fn null_params_read_as_an_empty_object_and_bad_params_carry_the_error() {
        let NoParams {} = parse_params(&Value::Null).unwrap();
        let verdict = parse_params::<NoParams>(&json!({"surprise": 1})).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
        assert!(verdict.raw["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("surprise"));
    }

    #[test]
    fn verdicts_merge_extra_fields_beside_the_reason_code() {
        let verdict = passed("ok", "fine".into(), json!({"rows": 2}));
        assert_eq!(
            verdict.raw,
            json!({"reason_code": "ok", "detail": "fine", "rows": 2})
        );
        assert_eq!(
            outcome(&failed("no", "bad".into(), Value::Null)),
            test_support::fail("no")
        );
    }
}
