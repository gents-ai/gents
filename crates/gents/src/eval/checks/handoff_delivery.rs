//! `handoff_delivery`: the runtime's handoff guarantees over a settled trial
//! home. Every violation is a harness failure, not the subject's.
//!
//! The subject configures the automation; the runtime delivers it. Once the
//! runtime accepted a trigger and fired it, the subject can no longer cause a
//! fire to lose its request, its outcome or its session, so a violation here
//! is [`OutcomeKind::Infrastructure`]: no evidence about the subject, and a
//! finding about Gents with the ids needed to reproduce it.

use std::collections::{BTreeMap, BTreeSet};

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::{
    bounded, excerpt, grader, Check, CheckDescription, CheckVerdict, EXCERPT_CHARS,
};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};
use crate::eval::OutcomeKind;

/// Params name the captures the check reads:
///
/// - `requests`: `AgentRequest` rows with `request_id`, `session_id`,
///   `lifecycle_state` (and `failure_reason` to quote);
/// - `fires`: `TriggerFire` rows with `fire_key`, `trigger_id`,
///   `source_doc_id`, `request_id`, `session_id`, `goal_id`, `emit_outcome`;
/// - `outcomes`: `FireOutcome` rows with `fire_key`, `request_id`,
///   `session_id`, `terminal_state`;
/// - `triggers`: `Trigger` rows with `trigger_id`, `session_id_template`,
///   `last_status`, `last_error`;
/// - `sources`: captures of the source collections, with `_docID`, whose
///   fields a `session_id_template` of the form `{{ doc.FIELD }}` names;
/// - `goals` (optional): `Goal` rows with `goal_id`, `status`;
/// - `runtime` (optional): `NodeRuntime` rows with `last_reconcile_error`.
///
/// Gates, each violation naming its ids: a request not terminal
/// (`request_not_terminal`); a fire with no request, or naming a request the
/// home lacks (`fire_without_request`); an outcome-emitting fire whose request
/// (and Goal, when it has one) ended with no `FireOutcome` for its key
/// (`fire_without_outcome`); a fire into `{{ doc.FIELD }}` that landed
/// anywhere but that field's value (`wrong_session`); an outcome naming
/// another session than its fire (`outcome_session`); a trigger or runtime
/// reconcile error (`runtime_rejected_config`).
pub struct HandoffDelivery;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    requests: String,
    fires: String,
    outcomes: String,
    triggers: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    goals: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
}

/// Violations quoted in the verdict before the rest are only counted.
const SHOWN: usize = 40;

/// Goal statuses after which a goal-backed fire owes its outcome.
const ENDED_GOAL: &[&str] = &["complete", "blocked", "budget_limited"];

fn str_field<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// `FIELD` of a `{{ doc.FIELD }}` template and nothing else.
fn doc_field(template: &str) -> Option<&str> {
    let inner = template
        .trim()
        .strip_prefix("{{")?
        .strip_suffix("}}")?
        .trim();
    let field = inner.strip_prefix("doc.")?;
    field
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then_some(field)
}

fn terminal(state: Option<&str>) -> bool {
    state
        .and_then(|state| RequestLifecycleState::parse(state).ok())
        .is_some_and(RequestLifecycleState::is_terminal)
}

struct Violations(Vec<(String, String)>);

impl Violations {
    fn push(&mut self, gate: &str, detail: String) {
        self.0.push((gate.to_owned(), detail));
    }
}

impl Check for HandoffDelivery {
    fn name(&self) -> &'static str {
        "handoff_delivery"
    }

    fn version(&self) -> &'static str {
        "2"
    }

    fn describe(&self) -> CheckDescription {
        let capture = json!({"type": "string", "description": "a documents capture name"});
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: "Harness gate over a settled home: every request terminal, every trigger fire has its request, every outcome-emitting fire whose work ended has its FireOutcome, fires into {{ doc.FIELD }} land in that session, and no trigger or runtime reconcile error. A violation is infrastructure (a Gents finding), never the subject's failure; with no fire at all the gate is inconclusive.".into(),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "requests": capture,
                    "fires": capture,
                    "outcomes": capture,
                    "triggers": capture,
                    "sources": {"type": "array", "items": capture},
                    "goals": capture,
                    "runtime": capture
                },
                "required": ["requests", "fires", "outcomes", "triggers"],
                "additionalProperties": false
            }),
            reads: vec!["capture:documents".into()],
            reason_codes: vec![
                ("delivered".into(), "every gate held over at least one fire".into()),
                ("untested".into(), "inconclusive: no trigger fired, so delivery was not exercised".into()),
                ("harness_gate_failed".into(), "infrastructure: a delivery guarantee failed; raw.violations names the ids".into()),
                ("missing_capture".into(), "grader: a required capture is absent".into()),
                ("bad_params".into(), "grader: params did not parse".into()),
            ],
        }
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string()),
        };
        let rows = |name: &str| match stage.captures.get(name) {
            Some(CaptureResult::Documents { rows }) => Some(rows.as_slice()),
            _ => None,
        };
        // A capture the params name is required. An optional one left out of
        // the params stays optional, but a named one that failed would
        // silently disable its gates.
        let required: Vec<&String> = [
            &params.requests,
            &params.fires,
            &params.outcomes,
            &params.triggers,
        ]
        .into_iter()
        .chain(params.goals.as_ref())
        .chain(params.runtime.as_ref())
        .chain(params.sources.iter())
        .collect();
        if let Some(missing) = required.iter().find(|name| rows(name).is_none()) {
            return grader(
                "missing_capture",
                format!("the stage produced no documents capture named {missing}"),
            );
        }
        let requests = rows(&params.requests).unwrap_or_default();
        let fires = rows(&params.fires).unwrap_or_default();
        let outcomes = rows(&params.outcomes).unwrap_or_default();
        let triggers = rows(&params.triggers).unwrap_or_default();

        let request_by_id: BTreeMap<&str, &Value> = requests
            .iter()
            .filter_map(|row| Some((str_field(row, "request_id")?, row)))
            .collect();
        let outcome_by_key: BTreeMap<&str, &Value> = outcomes
            .iter()
            .filter_map(|row| Some((str_field(row, "fire_key")?, row)))
            .collect();
        let template_by_trigger: BTreeMap<&str, &str> = triggers
            .iter()
            .filter_map(|row| {
                Some((
                    str_field(row, "trigger_id")?,
                    str_field(row, "session_id_template")?,
                ))
            })
            .collect();
        let source_by_doc: BTreeMap<&str, &Value> = params
            .sources
            .iter()
            .filter_map(|name| rows(name))
            .flatten()
            .filter_map(|row| Some((str_field(row, "_docID")?, row)))
            .collect();
        let goal_status: BTreeMap<&str, &str> = params
            .goals
            .as_deref()
            .and_then(rows)
            .unwrap_or_default()
            .iter()
            .filter_map(|row| Some((str_field(row, "goal_id")?, str_field(row, "status")?)))
            .collect();

        let mut violations = Violations(Vec::new());
        for row in requests {
            let state = str_field(row, "lifecycle_state");
            if !terminal(state) {
                violations.push(
                    "request_not_terminal",
                    format!(
                        "request {} in session {} is {}",
                        str_field(row, "request_id").unwrap_or("?"),
                        str_field(row, "session_id").unwrap_or("?"),
                        state.unwrap_or("absent")
                    ),
                );
            }
        }
        for fire in fires {
            let key = str_field(fire, "fire_key").unwrap_or("?");
            let trigger = str_field(fire, "trigger_id").unwrap_or("?");
            let Some(request_id) = str_field(fire, "request_id") else {
                violations.push(
                    "fire_without_request",
                    format!("fire {key} of trigger {trigger} has no request"),
                );
                continue;
            };
            let Some(request) = request_by_id.get(request_id) else {
                violations.push(
                    "fire_without_request",
                    format!("fire {key} of trigger {trigger} names request {request_id}, which the home lacks"),
                );
                continue;
            };
            let fire_session = str_field(fire, "session_id");
            let request_session = str_field(request, "session_id");
            if let Some(field) = template_by_trigger.get(trigger).and_then(|t| doc_field(t)) {
                let expected = str_field(fire, "source_doc_id")
                    .and_then(|doc| source_by_doc.get(doc))
                    .and_then(|source| str_field(source, field));
                if let Some(expected) = expected {
                    if request_session != Some(expected) || fire_session != Some(expected) {
                        violations.push(
                            "wrong_session",
                            format!(
                                "fire {key} of trigger {trigger} targets doc.{field} = {expected}, but its fire names session {} and its request {request_id} ran in {}",
                                fire_session.unwrap_or("absent"),
                                request_session.unwrap_or("absent")
                            ),
                        );
                    }
                }
            }
            let emits = fire.get("emit_outcome").and_then(Value::as_bool) == Some(true);
            let work_ended = terminal(str_field(request, "lifecycle_state"))
                && str_field(fire, "goal_id").is_none_or(|goal| {
                    goal_status
                        .get(goal)
                        .is_some_and(|status| ENDED_GOAL.contains(status))
                });
            match outcome_by_key.get(key) {
                None if emits && work_ended => violations.push(
                    "fire_without_outcome",
                    format!(
                        "fire {key} of trigger {trigger} emits outcomes and its work ended (request {request_id}), but no FireOutcome carries its key"
                    ),
                ),
                Some(outcome) => {
                    let outcome_session = str_field(outcome, "session_id");
                    if outcome_session.is_some() && outcome_session != fire_session {
                        violations.push(
                            "outcome_session",
                            format!(
                                "outcome of fire {key} names session {}, its fire {}",
                                outcome_session.unwrap_or("absent"),
                                fire_session.unwrap_or("absent")
                            ),
                        );
                    }
                }
                None => {}
            }
        }
        // `last_error` also carries an ordinary skip (a serial trigger busy
        // with prior work); only an `error` status is a rejection.
        for row in triggers {
            if str_field(row, "last_status") != Some("error") {
                continue;
            }
            if let Some(error) = str_field(row, "last_error") {
                violations.push(
                    "runtime_rejected_config",
                    format!(
                        "trigger {} last_error: {}",
                        str_field(row, "trigger_id").unwrap_or("?"),
                        excerpt(error, EXCERPT_CHARS)
                    ),
                );
            }
        }
        for row in params.runtime.as_deref().and_then(rows).unwrap_or_default() {
            if let Some(error) = str_field(row, "last_reconcile_error") {
                violations.push(
                    "runtime_rejected_config",
                    format!("runtime reconcile error: {}", excerpt(error, EXCERPT_CHARS)),
                );
            }
        }

        let mut by_gate: BTreeMap<&str, usize> = BTreeMap::new();
        for (gate, _) in &violations.0 {
            *by_gate.entry(gate.as_str()).or_default() += 1;
        }
        let triggers_fired: BTreeSet<&str> = fires
            .iter()
            .filter_map(|fire| str_field(fire, "trigger_id"))
            .collect();
        let counts = json!({
            "requests": requests.len(),
            "fires": fires.len(),
            "outcomes": outcomes.len(),
            "triggers_fired": triggers_fired.len(),
        });
        if !violations.0.is_empty() {
            let shown: Vec<Value> = violations
                .0
                .iter()
                .take(SHOWN)
                .map(|(gate, detail)| json!({"gate": gate, "detail": detail}))
                .collect();
            let mut text = format!("{} delivery violations:\n", violations.0.len());
            for (gate, detail) in violations.0.iter().take(12) {
                text.push_str(&format!("- {gate}: {detail}\n"));
            }
            return CheckVerdict {
                kind: OutcomeKind::Infrastructure,
                score_bp: None,
                raw: json!({
                    "reason_code": "harness_gate_failed",
                    "by_gate": by_gate,
                    "violations": shown,
                    "counts": counts,
                }),
                feedback: Some(bounded(text)),
            };
        }
        if fires.is_empty() {
            return CheckVerdict {
                kind: OutcomeKind::Inconclusive,
                score_bp: None,
                raw: json!({"reason_code": "untested", "counts": counts}),
                feedback: None,
            };
        }
        CheckVerdict {
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            raw: json!({"reason_code": "delivered", "counts": counts}),
            feedback: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::scripted::ScriptedExecutor;

    fn stage(captures: &[(&str, Vec<Value>)]) -> StageEvidence {
        let mut stage = ScriptedExecutor::passed_evidence("did:x", "s1", "requests", Vec::new())
            .stages
            .remove(0);
        stage.captures.clear();
        for (name, rows) in captures {
            stage.captures.insert(
                (*name).to_string(),
                CaptureResult::Documents { rows: rows.clone() },
            );
        }
        stage
    }

    fn params() -> Value {
        json!({
            "requests": "requests", "fires": "fires", "outcomes": "outcomes",
            "triggers": "triggers", "sources": ["results"], "goals": "goals",
            "runtime": "runtime"
        })
    }

    fn home(outcome_session: &str, request_session: &str, state: &str) -> StageEvidence {
        stage(&[
            (
                "requests",
                vec![
                    json!({"request_id": "r1", "session_id": request_session, "lifecycle_state": state}),
                ],
            ),
            (
                "fires",
                vec![json!({
                    "fire_key": "k1", "trigger_id": "inbox", "source_doc_id": "bae-1",
                    "request_id": "r1", "session_id": "lead", "goal_id": null, "emit_outcome": true
                })],
            ),
            (
                "outcomes",
                vec![json!({"fire_key": "k1", "request_id": "r1", "session_id": outcome_session})],
            ),
            (
                "triggers",
                vec![
                    json!({"trigger_id": "inbox", "session_id_template": "{{ doc.reply_session_id }}", "last_error": null}),
                ],
            ),
            (
                "results",
                vec![json!({"_docID": "bae-1", "reply_session_id": "lead"})],
            ),
            ("goals", vec![]),
            ("runtime", vec![json!({"last_reconcile_error": null})]),
        ])
    }

    #[test]
    fn a_delivered_fire_passes() {
        let verdict = HandoffDelivery.evaluate(&params(), &home("lead", "lead", "completed"));
        assert_eq!(verdict.kind, OutcomeKind::Passed, "{}", verdict.raw);
        assert_eq!(verdict.raw["counts"]["fires"], 1);
    }

    #[test]
    fn each_gate_is_infrastructure_with_its_ids() {
        let verdict = HandoffDelivery.evaluate(&params(), &home("elsewhere", "other", "running"));
        assert_eq!(verdict.kind, OutcomeKind::Infrastructure);
        assert_eq!(verdict.score_bp, None);
        assert_eq!(verdict.raw["reason_code"], "harness_gate_failed");
        assert_eq!(verdict.raw["by_gate"]["request_not_terminal"], 1);
        assert_eq!(verdict.raw["by_gate"]["wrong_session"], 1);
        assert_eq!(verdict.raw["by_gate"]["outcome_session"], 1);
        assert!(verdict.feedback.unwrap().contains("r1"));
    }

    #[test]
    fn a_missing_outcome_is_owed_only_once_the_work_ended() {
        let mut evidence = home("lead", "lead", "completed");
        evidence
            .captures
            .insert("outcomes".into(), CaptureResult::Documents { rows: vec![] });
        let verdict = HandoffDelivery.evaluate(&params(), &evidence);
        assert_eq!(
            verdict.raw["by_gate"]["fire_without_outcome"], 1,
            "{}",
            verdict.raw
        );

        // A goal-backed fire whose Goal is still active owes nothing yet.
        let CaptureResult::Documents { rows } = evidence.captures.get_mut("fires").unwrap() else {
            unreachable!()
        };
        rows[0]["goal_id"] = json!("g1");
        evidence.captures.insert(
            "goals".into(),
            CaptureResult::Documents {
                rows: vec![json!({"goal_id": "g1", "status": "active"})],
            },
        );
        let verdict = HandoffDelivery.evaluate(&params(), &evidence);
        assert_eq!(verdict.kind, OutcomeKind::Passed, "{}", verdict.raw);
    }

    #[test]
    fn no_fire_is_inconclusive_and_a_reconcile_error_is_a_gate() {
        let quiet = stage(&[
            ("requests", vec![]),
            ("fires", vec![]),
            ("outcomes", vec![]),
            ("triggers", vec![]),
            ("results", vec![]),
            ("goals", vec![]),
            ("runtime", vec![]),
        ]);
        let verdict = HandoffDelivery.evaluate(&params(), &quiet);
        assert_eq!(verdict.kind, OutcomeKind::Inconclusive);

        let mut rejected = quiet.clone();
        rejected.captures.insert(
            "triggers".into(),
            CaptureResult::Documents {
                rows: vec![
                    json!({"trigger_id": "t", "last_status": "error", "last_error": "template renders doc.missing"}),
                    json!({"trigger_id": "s", "last_status": "skipped", "last_error": "serial trigger busy"}),
                ],
            },
        );
        let verdict = HandoffDelivery.evaluate(&params(), &rejected);
        assert_eq!(verdict.raw["by_gate"]["runtime_rejected_config"], 1);
    }

    #[test]
    fn a_named_optional_capture_is_required_once_named() {
        let mut evidence = home("lead", "lead", "completed");
        evidence.captures.remove("goals");
        let verdict = HandoffDelivery.evaluate(&params(), &evidence);
        assert_eq!(
            verdict.raw["reason_code"], "missing_capture",
            "{}",
            verdict.raw
        );
    }

    #[test]
    fn a_missing_required_capture_is_the_grader() {
        let verdict = HandoffDelivery.evaluate(&params(), &stage(&[]));
        assert_eq!(verdict.kind, OutcomeKind::Grader);
        assert_eq!(verdict.raw["reason_code"], "missing_capture");
    }

    #[test]
    fn only_a_bare_doc_field_template_names_a_session_field() {
        assert_eq!(
            doc_field("{{ doc.reply_session_id }}"),
            Some("reply_session_id")
        );
        assert_eq!(
            doc_field("{{doc.target_session_id}}"),
            Some("target_session_id")
        );
        assert_eq!(doc_field("{{ doc.a | default('x') }}"), None);
        assert_eq!(doc_field("session-{{ doc.a }}"), None);
    }
}
